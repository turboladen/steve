mod render;
pub use render::render_sidebar;

use crate::{
    diagnostics::DiagnosticSummary,
    task::{Priority, TaskKind, TaskStatus},
    ui::message_block::{DiffContent, DiffLine},
};

/// Maximum number of tasks shown in the sidebar.
pub const MAX_SIDEBAR_TASKS: usize = 10;

/// Shorten a path to fit within `max_width` by abbreviating directory names
/// to their first character. The filename is always kept in full.
/// Returns the original path if it already fits.
pub(super) fn shorten_path(path: &str, max_width: usize) -> String {
    if path.chars().count() <= max_width {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return path.to_string(); // No directories to shorten
    }
    let filename = parts.last().expect("checked len > 1 above");
    let shortened_dirs: Vec<String> = parts[..parts.len() - 1]
        .iter()
        .map(|d| d.chars().next().map(String::from).unwrap_or_default())
        .collect();
    let mut result = shortened_dirs.join("/");
    result.push('/');
    result.push_str(filename);
    // If still too wide (long filename), truncate with ellipsis
    if result.chars().count() > max_width {
        if max_width <= 1 {
            return result.chars().take(max_width).collect();
        }
        let truncated: String = result.chars().take(max_width.saturating_sub(1)).collect();
        return format!("{truncated}\u{2026}"); // '…'
    }
    result
}

/// Maximum characters for a task title on a single sidebar line.
/// Derived at render time from sidebar width.
pub(super) fn max_task_title_chars(sidebar_width: usize) -> usize {
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
///
/// Backed by `LspStatusEntry` in `src/lsp/mod.rs`, which is the single source
/// of truth. The sidebar refreshes this on each `AppEvent::Tick` by polling
/// the shared status cache directly via `LspManager::snapshot_cache`
/// (bypassing `RwLock<LspManager>` — see `App::lsp_status_cache`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarLsp {
    /// Binary name of the LSP server (e.g., "rust-analyzer", "ty", "ruff").
    pub binary: String,
    /// Current lifecycle state: Starting / Indexing / Ready / Error.
    pub state: crate::lsp::LspServerState,
    /// Latest `$/progress` message. Rendered as a dimmed indented line below
    /// the server line during active states (Starting/Indexing).
    pub progress_message: Option<String>,
    /// When the next restart attempt will fire (only set during `Restarting`).
    pub next_restart_at: Option<std::time::Instant>,
}

/// MCP server status for sidebar display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarMcp {
    /// Server ID from config (e.g., "github", "atlassian").
    pub server_id: String,
    /// Number of tools provided by this server.
    pub tool_count: usize,
    /// Number of resources provided by this server.
    pub resource_count: usize,
    /// Number of prompts provided by this server.
    pub prompt_count: usize,
    /// Whether the server is currently connected.
    pub connected: bool,
    /// Error message if connection failed.
    pub error: Option<String>,
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
#[derive(Default)]
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
    /// MCP servers configured for the project.
    pub mcp_servers: Vec<SidebarMcp>,
    /// Current git branch name (None if not in a git repo).
    pub git_branch: Option<String>,
    /// Whether the repo has uncommitted changes (None if not in a git repo).
    pub git_dirty: Option<bool>,
    /// Repository name (last path component of repo root).
    pub git_repo_name: Option<String>,
    /// Diagnostics summary for sidebar indicator.
    pub diagnostics_summary: DiagnosticSummary,
    /// Current frame index for animated sidebar spinners (LSP Starting/Indexing).
    /// Advanced on every `AppEvent::Tick`; all animated entries share this index
    /// so they blink in lockstep.
    pub spinner_frame: usize,
}

impl SidebarState {
    /// Advance the shared sidebar spinner one frame. Called on each `AppEvent::Tick`.
    pub fn advance_spinner(&mut self) {
        use crate::ui::status_line::SPINNER_FRAMES;
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
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
        assert!(
            result.chars().count() <= 80,
            "should truncate to ~80 chars, got len {}",
            result.chars().count()
        );
        assert!(!result.ends_with(' '), "should not end with space");
    }

    #[test]
    fn summarize_description_multibyte_no_panic() {
        // 40 ASCII + 50 é (2 bytes each) = 90 chars, 140 bytes — must not panic on slice
        let desc = "a".repeat(40) + &"\u{00e9}".repeat(50);
        let result = summarize_description(&desc).unwrap();
        assert!(
            result.chars().count() <= 80,
            "should truncate multi-byte text safely"
        );
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
        assert_eq!(
            result,
            Some("Single sentence ending with period".to_string())
        );
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
    fn default_sidebar_state_has_no_git_info() {
        let state = SidebarState::default();
        assert!(state.git_branch.is_none());
        assert!(state.git_dirty.is_none());
        assert!(state.git_repo_name.is_none());
    }

    #[test]
    fn default_sidebar_state_has_no_lsp_servers() {
        let state = SidebarState::default();
        assert!(state.lsp_servers.is_empty());
    }

    #[test]
    fn default_sidebar_state_has_no_mcp_servers() {
        let state = SidebarState::default();
        assert!(state.mcp_servers.is_empty());
    }

    // -- shorten_path tests --

    #[test]
    fn shorten_path_already_fits() {
        assert_eq!(shorten_path("src/main.rs", 20), "src/main.rs");
    }

    #[test]
    fn shorten_path_deeply_nested() {
        let path = "src/components/dialogs/widgets/chart.rs";
        let shortened = shorten_path(path, 20);
        assert_eq!(shortened, "s/c/d/w/chart.rs");
    }

    #[test]
    fn shorten_path_no_directories() {
        assert_eq!(shorten_path("main.rs", 5), "main.rs");
    }

    #[test]
    fn shorten_path_single_directory() {
        // "s/file.rs" is 9 chars, fits in 10 but not 5
        assert_eq!(shorten_path("src/file.rs", 10), "s/file.rs");
    }

    #[test]
    fn shorten_path_single_directory_truncates_when_still_too_long() {
        // max_width=5, "s/file.rs" (9 chars) still too long → truncated with ellipsis
        let result = shorten_path("src/file.rs", 5);
        assert_eq!(result.chars().count(), 5);
        assert!(
            result.ends_with('\u{2026}'),
            "should end with ellipsis, got: {result}"
        );
    }

    #[test]
    fn shorten_path_empty() {
        assert_eq!(shorten_path("", 10), "");
    }

    #[test]
    fn shorten_path_exact_fit() {
        let path = "src/main.rs"; // 11 chars
        assert_eq!(shorten_path(path, 11), "src/main.rs");
    }

    #[test]
    fn shorten_path_one_over() {
        let path = "src/main.rs"; // 11 chars
        let shortened = shorten_path(path, 10);
        assert_eq!(shortened, "s/main.rs");
    }

    #[test]
    fn shorten_path_zero_max_width() {
        // max_width=0 should not panic, returns empty
        let result = shorten_path("src/main.rs", 0);
        assert_eq!(result.chars().count(), 0);
    }

    #[test]
    fn shorten_path_filename_exceeds_budget() {
        // Filename alone is longer than max_width — should truncate with ellipsis
        let result = shorten_path("src/very_long_filename_here.rs", 10);
        assert!(
            result.chars().count() <= 10,
            "result should fit in max_width, got: {result} ({})",
            result.chars().count()
        );
        assert!(
            result.ends_with('\u{2026}'),
            "should end with ellipsis, got: {result}"
        );
    }

    // -- spinner frame advancement --

    #[test]
    fn advance_spinner_wraps_at_end() {
        use crate::ui::status_line::SPINNER_FRAMES;
        let mut state = SidebarState {
            spinner_frame: SPINNER_FRAMES.len() - 1,
            ..Default::default()
        };
        state.advance_spinner();
        assert_eq!(state.spinner_frame, 0, "spinner should wrap to 0");
    }

    #[test]
    fn advance_spinner_from_zero_increments() {
        let mut state = SidebarState::default();
        assert_eq!(state.spinner_frame, 0);
        state.advance_spinner();
        assert_eq!(state.spinner_frame, 1);
        state.advance_spinner();
        assert_eq!(state.spinner_frame, 2);
    }

    // -- SidebarLsp equality considers state --

    #[test]
    fn sidebar_lsp_equality_considers_state() {
        let ready = SidebarLsp {
            binary: "rust-analyzer".into(),
            state: crate::lsp::LspServerState::Ready,
            progress_message: None,
            next_restart_at: None,
        };
        let starting = SidebarLsp {
            binary: "rust-analyzer".into(),
            state: crate::lsp::LspServerState::Starting,
            progress_message: None,
            next_restart_at: None,
        };
        assert_ne!(ready, starting, "state change must affect equality");
    }
}
