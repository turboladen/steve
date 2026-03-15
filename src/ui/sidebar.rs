use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

use crate::diagnostics::{DiagnosticSummary, Severity};
use crate::task::types::{Priority, TaskKind, TaskStatus};
use crate::ui::message_block::{DiffContent, DiffLine};

use super::primitives;
use super::status_line::format_tokens;
use super::theme::Theme;

/// Maximum number of tasks shown in the sidebar.
pub const MAX_SIDEBAR_TASKS: usize = 10;

/// Maximum characters for a task title on a single sidebar line.
/// Derived at render time from sidebar width.
fn max_task_title_chars(sidebar_width: usize) -> usize {
    sidebar_width.saturating_sub(6) // 1 indent + 3 icon + 2 margin
}

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

/// LSP server status for sidebar display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarLsp {
    /// Binary name of the LSP server (e.g., "rust-analyzer", "ty", "ruff").
    pub binary: String,
    /// Whether the server is currently running.
    pub running: bool,
}

/// Lightweight task summary for sidebar display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarTask {
    /// Task ID (e.g., "steve-ta3f" or "steve-b01c").
    pub id: String,
    /// Whether this is a task or a bug.
    pub kind: TaskKind,
    /// Human-readable title.
    pub title: String,
    /// Priority level.
    pub priority: Priority,
    /// Current lifecycle status.
    pub status: TaskStatus,
    /// Short summary from description (first sentence or ~80 chars).
    pub summary: Option<String>,
}

/// Extract a short summary from a task description: first sentence or first ~80 chars.
fn summarize_description(desc: &str) -> Option<String> {
    let trimmed = desc.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Take first sentence (up to first `. ` or `.\n` or end-of-string `.`)
    let first_sentence = trimmed
        .find(". ")
        .or_else(|| trimmed.find(".\n"))
        .or_else(|| {
            if trimmed.ends_with('.') {
                Some(trimmed.len() - 1)
            } else {
                None
            }
        });
    let snippet = match first_sentence {
        // pos is a byte offset from find() — compare character count, not bytes
        Some(pos) if trimmed[..pos].chars().count() <= 80 => &trimmed[..pos],
        _ => {
            // No short sentence — take first ~80 chars at a word boundary
            if trimmed.chars().count() <= 80 {
                trimmed
            } else {
                // Find byte offset of the 80th character (safe for multi-byte)
                let char80 = trimmed
                    .char_indices()
                    .nth(80)
                    .map(|(i, _)| i)
                    .unwrap_or(trimmed.len());
                let boundary = trimmed[..char80].rfind(' ').unwrap_or(char80);
                &trimmed[..boundary]
            }
        }
    };
    let snippet = snippet.trim();
    if snippet.is_empty() {
        None
    } else {
        Some(snippet.to_string())
    }
}

impl From<crate::task::Task> for SidebarTask {
    fn from(t: crate::task::Task) -> Self {
        let summary = t.description.as_deref().and_then(summarize_description);
        Self {
            id: t.id,
            kind: t.kind,
            title: t.title,
            priority: t.priority,
            status: t.status,
            summary,
        }
    }
}

/// State for the sidebar panel.
pub struct SidebarState {
    pub session_title: String,
    pub model_name: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub session_cost: Option<f64>,
    /// Tasks to display in the sidebar (open + session-closed, capped at MAX_SIDEBAR_TASKS).
    pub tasks: Vec<SidebarTask>,
    /// Task IDs completed during this session (shown with Done status in sidebar).
    pub session_closed_task_ids: Vec<String>,
    /// Accumulated file changes from write tools this session.
    pub changes: Vec<FileChange>,
    /// LSP servers detected in the project.
    pub lsp_servers: Vec<SidebarLsp>,
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
    /// Diagnostics summary for sidebar indicator.
    pub diagnostics_summary: DiagnosticSummary,
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
            tasks: Vec::new(),
            session_closed_task_ids: Vec::new(),
            changes: Vec::new(),
            lsp_servers: Vec::new(),
            git_branch: None,
            git_dirty: None,
            git_repo_name: None,
            context_window: 0,
            last_prompt_tokens: 0,
            diagnostics_summary: DiagnosticSummary::default(),
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

    /// Record a task ID as completed during this session (dedup on insert).
    pub fn record_task_closed(&mut self, id: String) {
        if !self.session_closed_task_ids.contains(&id) {
            self.session_closed_task_ids.push(id);
        }
    }
}

/// Maximum visible characters for branch name (derived from sidebar width).
fn max_branch_display(sidebar_width: usize) -> usize {
    sidebar_width.saturating_sub(2) // 1 indent + 1 margin
}

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
    // Sidebar content width (area minus 1-char left padding)
    let sidebar_width = area.width.saturating_sub(1) as usize;

    // -- Session section (top — most important context) --
    lines.push(primitives::section_header("Session", header_color));
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
    lines.push(primitives::section_separator(sidebar_width, theme));

    // -- Git section (below session, only when branch is known) --
    if let Some(branch) = &state.git_branch {
        lines.push(primitives::section_header("Git", header_color));
        if let Some(repo_name) = &state.git_repo_name {
            lines.push(Line::from(Span::styled(
                format!(" {repo_name}"),
                Style::default().fg(theme.dim),
            )));
        }
        let max_branch = max_branch_display(sidebar_width);
        let truncated_branch = if max_branch > 0 && branch.chars().count() > max_branch {
            let s: String = branch.chars().take(max_branch.saturating_sub(1)).collect();
            format!("{s}…")
        } else {
            branch.clone()
        };
        lines.push(Line::from(Span::styled(
            format!(" {truncated_branch}"),
            Style::default().fg(theme.fg),
        )));
        if let Some(dirty) = state.git_dirty {
            let (status_text, status_color) = if dirty {
                ("dirty", theme.warning)
            } else {
                ("clean", theme.success)
            };
            lines.push(Line::from(Span::styled(
                format!(" {status_text}"),
                Style::default().fg(status_color),
            )));
        }
        lines.push(primitives::section_separator(sidebar_width, theme));
    }

    // -- LSP section (if any servers detected) --
    if !state.lsp_servers.is_empty() {
        lines.push(primitives::section_header("LSP", header_color));
        for server in &state.lsp_servers {
            let (icon, icon_color) = if server.running {
                ("\u{25cf}", theme.success) // ● green — running
            } else {
                ("\u{25cb}", theme.dim) // ○ dim — detected but not running
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
                Span::styled(server.binary.clone(), Style::default().fg(theme.fg)),
            ]));
        }
        lines.push(primitives::section_separator(sidebar_width, theme));
    }

    // -- Health section (header + separate status line) --
    {
        lines.push(primitives::section_header("Health", header_color));
        let summary = &state.diagnostics_summary;
        let max_sev = summary.max_severity();
        let (icon, icon_color, label) = match max_sev {
            Severity::Error => (
                "\u{25cf}",
                theme.error,
                format!("{} errors", summary.error_count),
            ),
            Severity::Warning => (
                "\u{25cf}",
                theme.warning,
                format!("{} warnings", summary.warning_count),
            ),
            Severity::Info if summary.info_count > 0 => (
                "\u{2139}",
                theme.dim,
                format!("{} info", summary.info_count),
            ),
            Severity::Info => ("\u{2713}", theme.success, "ok".into()),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
            Span::styled(label, Style::default().fg(theme.dim)),
        ]));
        lines.push(primitives::section_separator(sidebar_width, theme));
    }

    // -- Changes section (if any files were modified) --
    if !state.changes.is_empty() {
        lines.push(primitives::section_header("Changes", header_color));
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
        lines.push(primitives::section_separator(sidebar_width, theme));
    }

    // -- Tasks section (if any tasks to show) --
    if !state.tasks.is_empty() {
        let open_count = state
            .tasks
            .iter()
            .filter(|t| t.status != TaskStatus::Done)
            .count();
        let done_count = state.tasks.len() - open_count;

        // Header with summary counts
        let mut header_parts = format!("Tasks ({open_count} open");
        if done_count > 0 {
            header_parts.push_str(&format!(", {done_count} done"));
        }
        header_parts.push(')');
        lines.push(primitives::section_header(&header_parts, header_color));

        // Individual task lines (2-3 lines each, pre-capped at MAX_SIDEBAR_TASKS by update_sidebar)
        for task in &state.tasks {
            let (icon, icon_color) = match (task.kind, task.status) {
                (_, TaskStatus::Done) => ("\u{2713}", theme.success),           // ✓
                (TaskKind::Bug, TaskStatus::Open) => ("\u{2298}", theme.error), // ⊘
                (TaskKind::Bug, TaskStatus::InProgress) => ("\u{2298}", theme.accent), // ⊘
                (TaskKind::Task, TaskStatus::Open) => ("\u{25cb}", theme.dim),  // ○
                (TaskKind::Task, TaskStatus::InProgress) => ("\u{25cf}", theme.accent), // ●
            };
            // Line 1: icon + short ID
            lines.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
                Span::styled(&task.id, Style::default().fg(theme.dim)),
            ]));
            // Line 2: truncated title
            let max_title = max_task_title_chars(sidebar_width);
            let title = if max_title > 3 && task.title.chars().count() > max_title {
                let truncated: String = task.title.chars().take(max_title.saturating_sub(3)).collect();
                format!("{truncated}...")
            } else {
                task.title.clone()
            };
            let title_color = if task.status == TaskStatus::Done {
                theme.dim
            } else {
                theme.fg
            };
            lines.push(Line::from(Span::styled(
                format!("   {title}"),
                Style::default().fg(title_color),
            )));
            // Line 3: summary (if available) — same indent as title, same width limit
            if let Some(summary) = &task.summary {
                let max_summary = max_task_title_chars(sidebar_width);
                let display_summary = if max_summary > 3 && summary.chars().count() > max_summary {
                    let truncated: String = summary.chars().take(max_summary.saturating_sub(3)).collect();
                    format!("{truncated}...")
                } else {
                    summary.clone()
                };
                lines.push(Line::from(Span::styled(
                    format!("   {display_summary}"),
                    Style::default().fg(theme.dim),
                )));
            }
        }
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

    #[test]
    fn sidebar_state_default_has_empty_tasks() {
        let state = SidebarState::default();
        assert!(state.tasks.is_empty());
        assert!(state.session_closed_task_ids.is_empty());
    }

    // -- record_task_closed tests --

    #[test]
    fn record_task_closed_adds_id() {
        let mut state = SidebarState::default();
        state.record_task_closed("task-abc".into());
        assert_eq!(state.session_closed_task_ids, vec!["task-abc"]);
    }

    #[test]
    fn record_task_closed_deduplicates() {
        let mut state = SidebarState::default();
        state.record_task_closed("task-abc".into());
        state.record_task_closed("task-abc".into());
        assert_eq!(state.session_closed_task_ids.len(), 1);
    }

    #[test]
    fn record_task_closed_preserves_order() {
        let mut state = SidebarState::default();
        state.record_task_closed("task-111".into());
        state.record_task_closed("task-222".into());
        state.record_task_closed("task-333".into());
        assert_eq!(
            state.session_closed_task_ids,
            vec!["task-111", "task-222", "task-333"]
        );
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

    fn make_sidebar_task(id: &str, title: &str, kind: TaskKind, priority: Priority, status: TaskStatus) -> SidebarTask {
        SidebarTask {
            id: id.to_string(),
            kind,
            title: title.to_string(),
            priority,
            status,
            summary: None,
        }
    }

    fn make_sidebar_task_with_summary(id: &str, title: &str, summary: &str, kind: TaskKind, priority: Priority, status: TaskStatus) -> SidebarTask {
        SidebarTask {
            id: id.to_string(),
            kind,
            title: title.to_string(),
            priority,
            status,
            summary: Some(summary.to_string()),
        }
    }

    #[test]
    fn buffer_sidebar_tasks_section_shows_individual_tasks() {
        let state = SidebarState {
            tasks: vec![
                make_sidebar_task("task-a1b2c3d4", "Fix sidebar rendering", TaskKind::Task, Priority::High, TaskStatus::Open),
                make_sidebar_task("task-e5f6g7h8", "Add new feature", TaskKind::Task, Priority::Medium, TaskStatus::InProgress),
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(text.contains("Tasks (2 open)"), "should show header with count, got:\n{text}");
        assert!(text.contains("task-a1b2c3d4"), "should show task ID");
        assert!(text.contains("Fix sidebar rendering"), "should show task title");
        assert!(text.contains("task-e5f6g7h8"), "should show second task ID");
        assert!(text.contains("Add new feature"), "should show second task title");
    }

    #[test]
    fn buffer_sidebar_tasks_section_shows_done_count() {
        let state = SidebarState {
            tasks: vec![
                make_sidebar_task("task-a1b2c3d4", "Open task", TaskKind::Task, Priority::Medium, TaskStatus::Open),
                make_sidebar_task("task-e5f6g7h8", "Done task", TaskKind::Task, Priority::Low, TaskStatus::Done),
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(text.contains("Tasks (1 open, 1 done)"), "should show open and done counts, got:\n{text}");
    }

    #[test]
    fn buffer_sidebar_tasks_truncates_long_titles() {
        let long_title = "This is a very long task title that should be truncated in the sidebar";
        let state = SidebarState {
            tasks: vec![make_sidebar_task("task-a1b2c3d4", long_title, TaskKind::Task, Priority::Medium, TaskStatus::Open)],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(text.contains("..."), "long title should be truncated with ellipsis");
        assert!(!text.contains(long_title), "full title should not appear");
    }

    #[test]
    fn buffer_sidebar_tasks_status_icons() {
        let state = SidebarState {
            tasks: vec![
                make_sidebar_task("task-open0001", "Open task", TaskKind::Task, Priority::Low, TaskStatus::Open),
                make_sidebar_task("task-prog0001", "Active task", TaskKind::Task, Priority::High, TaskStatus::InProgress),
                make_sidebar_task("task-done0001", "Finished task", TaskKind::Task, Priority::Medium, TaskStatus::Done),
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(text.contains("\u{25cb}"), "should show open icon (○)");
        assert!(text.contains("\u{25cf}"), "should show in-progress icon (●)");
        assert!(text.contains("\u{2713}"), "should show done icon (✓)");
    }

    #[test]
    fn buffer_sidebar_task_with_summary_renders_3_lines() {
        let state = SidebarState {
            tasks: vec![make_sidebar_task_with_summary(
                "task-sum00001",
                "Refactor sidebar",
                "Break into smaller fns",
                TaskKind::Task,
                Priority::Medium,
                TaskStatus::InProgress,
            )],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(text.contains("task-sum00001"), "should show task ID");
        assert!(text.contains("Refactor sidebar"), "should show title");
        assert!(
            text.contains("Break into smaller fns"),
            "should show summary, got:\n{text}"
        );
    }

    #[test]
    fn buffer_sidebar_task_without_summary_renders_2_lines() {
        let state = SidebarState {
            tasks: vec![make_sidebar_task(
                "task-nosm0001",
                "Simple task",
                TaskKind::Task,
                Priority::Low,
                TaskStatus::Open,
            )],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(text.contains("task-nosm0001"), "should show task ID");
        assert!(text.contains("Simple task"), "should show title");
        // No extra blank line where summary would be — just ID + title
    }

    #[test]
    fn buffer_sidebar_no_tasks_no_section() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(!text.contains("Tasks"), "no tasks = no 'Tasks' header");
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

    // -- LSP section tests --

    #[test]
    fn default_sidebar_state_has_no_lsp_servers() {
        let state = SidebarState::default();
        assert!(state.lsp_servers.is_empty());
    }

    #[test]
    fn buffer_sidebar_no_lsp_no_section() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(!text.contains("LSP"), "no LSP servers = no 'LSP' header");
    }

    #[test]
    fn buffer_sidebar_lsp_section_shows_running_servers() {
        let state = SidebarState {
            lsp_servers: vec![
                SidebarLsp { binary: "rust-analyzer".to_string(), running: true },
                SidebarLsp { binary: "ty".to_string(), running: true },
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("LSP"), "should show 'LSP' header");
        assert!(text.contains("rust-analyzer"), "should show rust-analyzer");
        assert!(text.contains("ty"), "should show ty");
        // Running servers get filled circle
        assert!(text.contains("\u{25cf}"), "running server should show ● icon");
    }

    #[test]
    fn buffer_sidebar_lsp_section_shows_not_running() {
        let state = SidebarState {
            lsp_servers: vec![
                SidebarLsp { binary: "rust-analyzer".to_string(), running: true },
                SidebarLsp { binary: "solargraph".to_string(), running: false },
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("\u{25cf}"), "running server should show ● icon");
        assert!(text.contains("\u{25cb}"), "not-running server should show ○ icon");
        assert!(text.contains("rust-analyzer"), "should show rust-analyzer");
        assert!(text.contains("solargraph"), "should show solargraph");
    }

    #[test]
    fn buffer_sidebar_lsp_renders_above_changes() {
        let mut state = SidebarState {
            lsp_servers: vec![
                SidebarLsp { binary: "rust-analyzer".to_string(), running: true },
            ],
            ..Default::default()
        };
        state.record_file_change("file.rs".into(), 1, 0);
        let text = render_sidebar_to_string(40, 25, &state);
        let lsp_pos = text.find("LSP").expect("LSP header not found");
        let changes_pos = text.find("Changes").expect("Changes header not found");
        assert!(lsp_pos < changes_pos, "LSP should render above Changes");
    }

    #[test]
    fn buffer_sidebar_lsp_renders_below_git() {
        let state = SidebarState {
            git_branch: Some("main".to_string()),
            git_dirty: Some(false),
            lsp_servers: vec![
                SidebarLsp { binary: "typescript-language-server".to_string(), running: true },
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 25, &state);
        let git_pos = text.find("Git").expect("Git header not found");
        let lsp_pos = text.find("LSP").expect("LSP header not found");
        assert!(git_pos < lsp_pos, "Git should render above LSP");
    }

    #[test]
    fn max_branch_display_values() {
        assert_eq!(max_branch_display(36), 34);
        assert_eq!(max_branch_display(44), 42);
        assert_eq!(max_branch_display(12), 10);
        assert_eq!(max_branch_display(0), 0);
    }

    #[test]
    fn max_task_title_chars_values() {
        assert_eq!(max_task_title_chars(36), 30);
        assert_eq!(max_task_title_chars(44), 38);
        assert_eq!(max_task_title_chars(6), 0);
        assert_eq!(max_task_title_chars(0), 0);
    }

    // -- summarize_description tests --

    #[test]
    fn summarize_description_first_sentence() {
        let result = summarize_description("Fix the sidebar. Then clean up tests.");
        assert_eq!(result, Some("Fix the sidebar".to_string()));
    }

    #[test]
    fn summarize_description_short_text_no_period() {
        let result = summarize_description("Quick fix for layout");
        assert_eq!(result, Some("Quick fix for layout".to_string()));
    }

    #[test]
    fn summarize_description_long_text_word_boundary() {
        let long = "This is a description that goes well beyond eighty characters and should be truncated at a word boundary somewhere around here";
        let result = summarize_description(long).unwrap();
        assert!(result.chars().count() <= 80, "should truncate to ~80 chars, got len {}", result.chars().count());
        assert!(!result.ends_with(' '), "should not end with space");
    }

    #[test]
    fn summarize_description_multibyte_no_panic() {
        // 40 ASCII + 50 é (2 bytes each) = 90 chars, 140 bytes — must not panic on slice
        let desc = "a".repeat(40) + &"\u{00e9}".repeat(50);
        let result = summarize_description(&desc).unwrap();
        assert!(result.chars().count() <= 80, "should truncate multi-byte text safely");
    }

    #[test]
    fn summarize_description_long_single_word() {
        // No spaces — falls back to truncating at exactly 80 chars
        let word = "x".repeat(100);
        let result = summarize_description(&word).unwrap();
        assert_eq!(result.chars().count(), 80);
    }

    #[test]
    fn summarize_description_empty_string() {
        assert_eq!(summarize_description(""), None);
    }

    #[test]
    fn summarize_description_whitespace_only() {
        assert_eq!(summarize_description("   \n  "), None);
    }

    #[test]
    fn summarize_description_trailing_period() {
        let result = summarize_description("Single sentence ending with period.");
        assert_eq!(result, Some("Single sentence ending with period".to_string()));
    }

    #[test]
    fn summarize_description_newline_sentence_boundary() {
        let result = summarize_description("First line.\nSecond line.");
        assert_eq!(result, Some("First line".to_string()));
    }

    // -- From<Task> summary extraction --

    #[test]
    fn sidebar_task_from_task_with_description() {
        let task = crate::task::Task {
            id: "steve-ta001".to_string(),
            kind: TaskKind::Task,
            title: "Test task".to_string(),
            description: Some("Implement the widget. Then test it.".to_string()),
            epic_id: None,
            session_id: None,
            priority: Priority::Medium,
            status: TaskStatus::Open,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let sidebar: SidebarTask = task.into();
        assert_eq!(sidebar.summary, Some("Implement the widget".to_string()));
    }

    #[test]
    fn sidebar_task_from_task_without_description() {
        let task = crate::task::Task {
            id: "steve-ta002".to_string(),
            kind: TaskKind::Task,
            title: "No desc".to_string(),
            description: None,
            epic_id: None,
            session_id: None,
            priority: Priority::Low,
            status: TaskStatus::Open,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let sidebar: SidebarTask = task.into();
        assert_eq!(sidebar.summary, None);
    }

    #[test]
    fn buffer_sidebar_task_no_priority_abbreviation() {
        let state = SidebarState {
            tasks: vec![make_sidebar_task(
                "task-nopr0001",
                "My task",
                TaskKind::Task,
                Priority::High,
                TaskStatus::Open,
            )],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        // Priority abbreviation was removed from line 1
        assert!(text.contains("task-nopr0001"), "should show task ID");
        // "hi" appears in "Health" header, so check it doesn't appear on the task ID line
        // by checking the line doesn't contain " hi" after the ID
        let id_line = text.lines().find(|l| l.contains("task-nopr0001")).unwrap();
        assert!(!id_line.contains(" hi"), "should not show priority abbreviation on ID line");
    }
}

