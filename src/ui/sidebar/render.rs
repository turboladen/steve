use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

use crate::{
    diagnostics::Severity,
    task::{TaskKind, TaskStatus},
    ui::{primitives, status_line::format_tokens, theme::Theme},
};

use super::{SidebarState, max_task_title_chars, shorten_path};

/// Maximum visible characters for branch name (derived from sidebar width).
fn max_branch_display(sidebar_width: usize) -> usize {
    sidebar_width.saturating_sub(2) // 1 indent + 1 margin
}

fn render_session_section<'a>(
    lines: &mut Vec<Line<'a>>,
    state: &'a SidebarState,
    theme: &'a Theme,
    sidebar_width: usize,
    header_color: ratatui::style::Color,
) {
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
    // Cumulative token breakdown: In/Out paired, Tot/Cost paired
    let in_str = format_tokens(state.prompt_tokens);
    let out_str = format_tokens(state.completion_tokens);
    lines.push(Line::from(Span::styled(
        format!(" In: {in_str:>5}  Out: {out_str:>5}"),
        Style::default().fg(theme.dim),
    )));
    let tot_str = format_tokens(state.total_tokens);
    let cost_str = match state.session_cost {
        Some(cost) => format!("${:.4}", cost),
        None => "N/A".into(),
    };
    lines.push(Line::from(Span::styled(
        format!(" Tot: {tot_str:>5} Cost: {cost_str}"),
        Style::default().fg(theme.dim),
    )));
    // Health status (merged from standalone section)
    let summary = &state.diagnostics_summary;
    let max_sev = summary.max_severity();
    let (health_icon, health_icon_color, health_label) = match max_sev {
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
        Span::styled(" Health: ", Style::default().fg(theme.dim)),
        Span::styled(
            format!("{health_icon} "),
            Style::default().fg(health_icon_color),
        ),
        Span::styled(health_label, Style::default().fg(theme.dim)),
    ]));
    lines.push(primitives::section_separator(sidebar_width, theme));
}

fn render_git_section<'a>(
    lines: &mut Vec<Line<'a>>,
    state: &'a SidebarState,
    theme: &'a Theme,
    sidebar_width: usize,
    header_color: ratatui::style::Color,
) {
    let branch = match &state.git_branch {
        Some(b) => b,
        None => return,
    };

    lines.push(primitives::section_header("Git", header_color));
    // Repo name + dirty/clean status on one line (when both available)
    match (&state.git_repo_name, state.git_dirty) {
        (Some(repo_name), Some(dirty)) => {
            let (icon, status_text, status_color) = if dirty {
                ("\u{25cf}", "dirty", theme.warning)
            } else {
                ("\u{25cf}", "clean", theme.success)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {repo_name} "), Style::default().fg(theme.dim)),
                Span::styled(format!("{icon} "), Style::default().fg(status_color)),
                Span::styled(status_text, Style::default().fg(status_color)),
            ]));
        }
        (Some(repo_name), None) => {
            lines.push(Line::from(Span::styled(
                format!(" {repo_name}"),
                Style::default().fg(theme.dim),
            )));
        }
        (None, Some(dirty)) => {
            let (icon, status_text, status_color) = if dirty {
                ("\u{25cf}", "dirty", theme.warning)
            } else {
                ("\u{25cf}", "clean", theme.success)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(status_color)),
                Span::styled(status_text, Style::default().fg(status_color)),
            ]));
        }
        (None, None) => {}
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
    // Inline file-change stats (formerly the standalone "Changes" section)
    if !state.changes.is_empty() {
        lines.push(Line::from(""));
        let mut sorted_changes = state.changes.clone();
        sorted_changes.sort_by(|a, b| a.path.cmp(&b.path));
        for change in &sorted_changes {
            let mut stats_width = 0;
            if change.additions > 0 {
                stats_width += 2 + format!("{}", change.additions).chars().count();
            }
            if change.removals > 0 {
                stats_width += 2 + format!("{}", change.removals).chars().count();
            }
            let path_width = sidebar_width.saturating_sub(1 + stats_width);
            let display_path = shorten_path(&change.path, path_width);
            let mut spans = vec![Span::styled(
                format!(" {display_path}"),
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
        let (total_add, total_rem) = state.total_changes();
        let file_word = if state.changes.len() == 1 {
            "file"
        } else {
            "files"
        };
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
    }
    lines.push(primitives::section_separator(sidebar_width, theme));
}

fn render_servers_section<'a>(
    lines: &mut Vec<Line<'a>>,
    state: &'a SidebarState,
    theme: &'a Theme,
    sidebar_width: usize,
    header_color: ratatui::style::Color,
) {
    // -- LSP section (if any servers detected) --
    if !state.lsp_servers.is_empty() {
        use crate::{lsp::LspServerState, ui::status_line::SPINNER_FRAMES};
        lines.push(primitives::section_header("LSP", header_color));
        let spinner_glyph = SPINNER_FRAMES[state.spinner_frame % SPINNER_FRAMES.len()].to_string();
        for server in &state.lsp_servers {
            // Exhaustive match — a new LspServerState variant will force an
            // update here per CLAUDE.md convention (no `_ =>` wildcard).
            let (glyph, color) = match &server.state {
                LspServerState::Starting => (spinner_glyph.clone(), theme.warning),
                LspServerState::Indexing => (spinner_glyph.clone(), theme.success),
                LspServerState::Ready => ("\u{25cf}".to_string(), theme.success),
                LspServerState::Error { .. } => ("\u{2715}".to_string(), theme.error),
            };
            let label = format!("{} ({})", server.binary, server.state.label());
            lines.push(Line::from(vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                Span::styled(label, Style::default().fg(theme.fg)),
            ]));
        }
        lines.push(primitives::section_separator(sidebar_width, theme));
    }

    // -- MCP section (if any servers configured) --
    if !state.mcp_servers.is_empty() {
        lines.push(primitives::section_header("MCP", header_color));
        for server in &state.mcp_servers {
            let (icon, icon_color) = if server.connected {
                ("\u{25cf}", theme.success) // ● green — connected
            } else {
                ("\u{25cb}", theme.dim) // ○ dim — disconnected (matches LSP pattern)
            };
            let label = if server.connected {
                let mut counts = Vec::new();
                if server.tool_count > 0 {
                    counts.push(format!("{}T", server.tool_count));
                }
                if server.resource_count > 0 {
                    counts.push(format!("{}R", server.resource_count));
                }
                if server.prompt_count > 0 {
                    counts.push(format!("{}P", server.prompt_count));
                }
                if counts.is_empty() {
                    server.server_id.clone()
                } else {
                    format!("{} ({})", server.server_id, counts.join(" "))
                }
            } else {
                let suffix = server.error.as_deref().unwrap_or("disconnected");
                format!("{} ({})", server.server_id, suffix)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
                Span::styled(label, Style::default().fg(theme.fg)),
            ]));
        }
        lines.push(primitives::section_separator(sidebar_width, theme));
    }

    // Health is now rendered inside render_session_section.
}

fn render_tasks_section<'a>(
    lines: &mut Vec<Line<'a>>,
    state: &'a SidebarState,
    theme: &'a Theme,
    sidebar_width: usize,
    header_color: ratatui::style::Color,
) {
    if state.tasks.is_empty() {
        return;
    }

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
            (_, TaskStatus::Done) => ("\u{2713}", theme.success), // ✓
            (TaskKind::Bug, TaskStatus::Open) => ("\u{2298}", theme.error), // ⊘
            (TaskKind::Bug, TaskStatus::InProgress) => ("\u{2298}", theme.accent), // ⊘
            (TaskKind::Task, TaskStatus::Open) => ("\u{25cb}", theme.dim), // ○
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
            let truncated: String = task
                .title
                .chars()
                .take(max_title.saturating_sub(3))
                .collect();
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
                let truncated: String = summary
                    .chars()
                    .take(max_summary.saturating_sub(3))
                    .collect();
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

    render_session_section(&mut lines, state, theme, sidebar_width, header_color);
    render_git_section(&mut lines, state, theme, sidebar_width, header_color);
    render_servers_section(&mut lines, state, theme, sidebar_width, header_color);
    render_tasks_section(&mut lines, state, theme, sidebar_width, header_color);

    // 1-char left padding via Block::padding keeps content off the vertical divider.
    let block = Block::default().padding(ratatui::widgets::Padding::new(1, 0, 0, 0));

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        task::{Priority, TaskKind, TaskStatus},
        ui::sidebar::{SidebarLsp, SidebarMcp, SidebarTask},
    };
    use ratatui::layout::Rect;

    /// Helper: render sidebar into a buffer and return the text as a single string.
    fn render_sidebar_to_string(width: u16, height: u16, state: &SidebarState) -> String {
        let theme = Theme::default();
        let buf = crate::ui::render_to_buffer(width, height, |frame| {
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
    fn buffer_sidebar_token_display_cumulative() {
        let state = SidebarState {
            prompt_tokens: 23400,
            completion_tokens: 6600,
            total_tokens: 30000,
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        // Verify each value appears on the correct labeled line
        let lines: Vec<&str> = text.lines().collect();
        let in_line = lines
            .iter()
            .find(|l| l.contains("In:"))
            .expect("should have In: line");
        assert!(
            in_line.contains("23.4k"),
            "In: line should show prompt tokens, got: {in_line}"
        );
        let out_line = lines
            .iter()
            .find(|l| l.contains("Out:"))
            .expect("should have Out: line");
        assert!(
            out_line.contains("6.6k"),
            "Out: line should show completion tokens, got: {out_line}"
        );
        let tot_line = lines
            .iter()
            .find(|l| l.contains("Tot:"))
            .expect("should have Tot: line");
        assert!(
            tot_line.contains("30.0k"),
            "Tot: line should show total tokens, got: {tot_line}"
        );
        assert!(!text.contains("Ctx:"), "should not show old Ctx: format");
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
        assert!(
            text.contains("Cost: N/A"),
            "should show N/A when cost not configured"
        );
    }

    #[test]
    fn buffer_sidebar_changes_inside_git_section() {
        let mut state = SidebarState {
            git_branch: Some("main".to_string()),
            git_dirty: Some(true),
            ..Default::default()
        };
        state.record_file_change("src/main.rs".into(), 10, 3);
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(
            !text.contains("Changes"),
            "no standalone 'Changes' header — merged into Git"
        );
        assert!(text.contains("Git"), "should show 'Git' header");
        assert!(text.contains("src/main.rs"), "should show file path");
        assert!(text.contains("+10"), "should show additions in green");
        assert!(text.contains("-3"), "should show removals in red");
    }

    fn make_sidebar_task(
        id: &str,
        title: &str,
        kind: TaskKind,
        priority: Priority,
        status: TaskStatus,
    ) -> SidebarTask {
        SidebarTask {
            id: id.to_string(),
            kind,
            title: title.to_string(),
            priority,
            status,
            summary: None,
        }
    }

    fn make_sidebar_task_with_summary(
        id: &str,
        title: &str,
        summary: &str,
        kind: TaskKind,
        priority: Priority,
        status: TaskStatus,
    ) -> SidebarTask {
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
                make_sidebar_task(
                    "task-a1b2c3d4",
                    "Fix sidebar rendering",
                    TaskKind::Task,
                    Priority::High,
                    TaskStatus::Open,
                ),
                make_sidebar_task(
                    "task-e5f6g7h8",
                    "Add new feature",
                    TaskKind::Task,
                    Priority::Medium,
                    TaskStatus::InProgress,
                ),
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(
            text.contains("Tasks (2 open)"),
            "should show header with count, got:\n{text}"
        );
        assert!(text.contains("task-a1b2c3d4"), "should show task ID");
        assert!(
            text.contains("Fix sidebar rendering"),
            "should show task title"
        );
        assert!(text.contains("task-e5f6g7h8"), "should show second task ID");
        assert!(
            text.contains("Add new feature"),
            "should show second task title"
        );
    }

    #[test]
    fn buffer_sidebar_tasks_section_shows_done_count() {
        let state = SidebarState {
            tasks: vec![
                make_sidebar_task(
                    "task-a1b2c3d4",
                    "Open task",
                    TaskKind::Task,
                    Priority::Medium,
                    TaskStatus::Open,
                ),
                make_sidebar_task(
                    "task-e5f6g7h8",
                    "Done task",
                    TaskKind::Task,
                    Priority::Low,
                    TaskStatus::Done,
                ),
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(
            text.contains("Tasks (1 open, 1 done)"),
            "should show open and done counts, got:\n{text}"
        );
    }

    #[test]
    fn buffer_sidebar_tasks_truncates_long_titles() {
        let long_title = "This is a very long task title that should be truncated in the sidebar";
        let state = SidebarState {
            tasks: vec![make_sidebar_task(
                "task-a1b2c3d4",
                long_title,
                TaskKind::Task,
                Priority::Medium,
                TaskStatus::Open,
            )],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(
            text.contains("..."),
            "long title should be truncated with ellipsis"
        );
        assert!(!text.contains(long_title), "full title should not appear");
    }

    #[test]
    fn buffer_sidebar_tasks_status_icons() {
        let state = SidebarState {
            tasks: vec![
                make_sidebar_task(
                    "task-open0001",
                    "Open task",
                    TaskKind::Task,
                    Priority::Low,
                    TaskStatus::Open,
                ),
                make_sidebar_task(
                    "task-prog0001",
                    "Active task",
                    TaskKind::Task,
                    Priority::High,
                    TaskStatus::InProgress,
                ),
                make_sidebar_task(
                    "task-done0001",
                    "Finished task",
                    TaskKind::Task,
                    Priority::Medium,
                    TaskStatus::Done,
                ),
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 30, &state);
        assert!(text.contains("\u{25cb}"), "should show open icon (○)");
        assert!(
            text.contains("\u{25cf}"),
            "should show in-progress icon (●)"
        );
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
    fn buffer_sidebar_changes_require_git_branch() {
        // Changes only render inside the Git section, so no git_branch = no changes shown
        let mut state = SidebarState::default();
        state.record_file_change("file.rs".into(), 1, 0);
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(
            !text.contains("file.rs"),
            "no git branch = changes not shown"
        );
    }

    #[test]
    fn buffer_sidebar_no_changes_no_header() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(
            !text.contains("Changes"),
            "no changes = no 'Changes' header"
        );
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
        assert!(
            text.contains("(untitled)"),
            "empty title should show '(untitled)'"
        );
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
    fn buffer_sidebar_git_section_includes_changes() {
        let mut state = SidebarState {
            git_branch: Some("main".to_string()),
            git_dirty: Some(false),
            ..Default::default()
        };
        state.record_file_change("file.rs".into(), 1, 0);
        let text = render_sidebar_to_string(40, 25, &state);
        assert!(text.contains("Git"), "should show Git header");
        assert!(
            text.contains("file.rs"),
            "changes should appear in Git section"
        );
        assert!(!text.contains("Changes"), "no standalone Changes header");
    }

    // -- LSP section tests --

    #[test]
    fn buffer_sidebar_no_lsp_no_section() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(!text.contains("LSP"), "no LSP servers = no 'LSP' header");
    }

    fn ready_lsp(binary: &str) -> SidebarLsp {
        SidebarLsp {
            binary: binary.to_string(),
            state: crate::lsp::LspServerState::Ready,
            progress_message: None,
        }
    }

    #[test]
    fn buffer_sidebar_lsp_section_shows_running_servers() {
        let state = SidebarState {
            lsp_servers: vec![ready_lsp("rust-analyzer"), ready_lsp("ty")],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(text.contains("LSP"), "should show 'LSP' header");
        assert!(text.contains("rust-analyzer"), "should show rust-analyzer");
        assert!(text.contains("ty"), "should show ty");
        assert!(text.contains("Ready"), "should show Ready label");
        // Ready servers get filled circle
        assert!(text.contains("\u{25cf}"), "Ready server should show ● icon");
    }

    #[test]
    fn buffer_sidebar_lsp_section_shows_starting_and_indexing_with_spinner() {
        use crate::ui::status_line::SPINNER_FRAMES;
        let state = SidebarState {
            lsp_servers: vec![
                SidebarLsp {
                    binary: "rust-analyzer".to_string(),
                    state: crate::lsp::LspServerState::Starting,
                    progress_message: None,
                },
                SidebarLsp {
                    binary: "pyright-langserver".to_string(),
                    state: crate::lsp::LspServerState::Indexing,
                    progress_message: None,
                },
            ],
            spinner_frame: 0,
            ..Default::default()
        };
        let text = render_sidebar_to_string(60, 20, &state);
        assert!(text.contains("Starting"), "should show Starting label");
        assert!(text.contains("Indexing"), "should show Indexing label");
        // Spinner glyph is in the rendered output for both states
        let spinner = SPINNER_FRAMES[0].to_string();
        assert!(
            text.contains(&spinner),
            "should show spinner glyph for animated states"
        );
    }

    #[test]
    fn buffer_sidebar_lsp_section_shows_error_glyph_for_error_state() {
        let state = SidebarState {
            lsp_servers: vec![SidebarLsp {
                binary: "rust-analyzer".to_string(),
                state: crate::lsp::LspServerState::Error {
                    reason: "not found on PATH".to_string(),
                },
                progress_message: None,
            }],
            ..Default::default()
        };
        let text = render_sidebar_to_string(50, 20, &state);
        assert!(text.contains("Error"), "should show Error label");
        assert!(
            text.contains("\u{2715}"),
            "Error state should show ✕ glyph, got: {text}"
        );
    }

    #[test]
    fn buffer_sidebar_changes_inside_git_not_after_lsp() {
        let mut state = SidebarState {
            git_branch: Some("main".to_string()),
            git_dirty: Some(true),
            lsp_servers: vec![ready_lsp("rust-analyzer")],
            ..Default::default()
        };
        state.record_file_change("file.rs".into(), 1, 0);
        let text = render_sidebar_to_string(40, 25, &state);
        // Changes appear inside Git (before LSP), not after LSP
        let file_pos = text.find("file.rs").expect("file.rs not found");
        let lsp_pos = text.find("LSP").expect("LSP header not found");
        assert!(
            file_pos < lsp_pos,
            "changes should appear inside Git section, before LSP"
        );
    }

    #[test]
    fn buffer_sidebar_lsp_renders_below_git() {
        let state = SidebarState {
            git_branch: Some("main".to_string()),
            git_dirty: Some(false),
            lsp_servers: vec![ready_lsp("typescript-language-server")],
            ..Default::default()
        };
        let text = render_sidebar_to_string(40, 25, &state);
        let git_pos = text.find("Git").expect("Git header not found");
        let lsp_pos = text.find("LSP").expect("LSP header not found");
        assert!(git_pos < lsp_pos, "Git should render above LSP");
    }

    // -- MCP section tests --

    #[test]
    fn buffer_sidebar_no_mcp_no_section() {
        let state = SidebarState::default();
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(!text.contains("MCP"), "no MCP servers = no 'MCP' header");
    }

    #[test]
    fn buffer_sidebar_mcp_section_shows_connected_servers() {
        let state = SidebarState {
            mcp_servers: vec![
                SidebarMcp {
                    server_id: "github".to_string(),
                    tool_count: 5,
                    resource_count: 0,
                    prompt_count: 0,
                    connected: true,
                    error: None,
                },
                SidebarMcp {
                    server_id: "atlassian".to_string(),
                    tool_count: 3,
                    resource_count: 2,
                    prompt_count: 0,
                    connected: true,
                    error: None,
                },
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(50, 20, &state);
        assert!(text.contains("MCP"), "should show 'MCP' header");
        assert!(text.contains("github"), "should show github server");
        assert!(text.contains("5T"), "should show tool count");
        assert!(text.contains("atlassian"), "should show atlassian server");
        assert!(text.contains("3T"), "should show tool count for atlassian");
        assert!(
            text.contains("2R"),
            "should show resource count for atlassian"
        );
        // Connected servers get filled circle
        assert!(
            text.contains("\u{25cf}"),
            "connected server should show \u{25cf} icon"
        );
    }

    #[test]
    fn buffer_sidebar_mcp_section_shows_disconnected() {
        let state = SidebarState {
            mcp_servers: vec![
                SidebarMcp {
                    server_id: "github".to_string(),
                    tool_count: 5,
                    resource_count: 0,
                    prompt_count: 0,
                    connected: true,
                    error: None,
                },
                SidebarMcp {
                    server_id: "broken".to_string(),
                    tool_count: 0,
                    resource_count: 0,
                    prompt_count: 0,
                    connected: false,
                    error: Some("timeout".to_string()),
                },
            ],
            ..Default::default()
        };
        let text = render_sidebar_to_string(50, 20, &state);
        assert!(
            text.contains("\u{25cf}"),
            "connected server should show \u{25cf} icon"
        );
        assert!(
            text.contains("\u{25cb}"),
            "disconnected server should show \u{25cb} icon"
        );
        assert!(text.contains("broken"), "should show broken server");
        assert!(text.contains("timeout"), "should show error message");
    }

    #[test]
    fn buffer_sidebar_mcp_disconnected_default_message() {
        let state = SidebarState {
            mcp_servers: vec![SidebarMcp {
                server_id: "broken".to_string(),
                tool_count: 0,
                resource_count: 0,
                prompt_count: 0,
                connected: false,
                error: None,
            }],
            ..Default::default()
        };
        let text = render_sidebar_to_string(50, 20, &state);
        assert!(
            text.contains("disconnected"),
            "should show default 'disconnected' label"
        );
    }

    #[test]
    fn buffer_sidebar_mcp_renders_below_lsp() {
        let state = SidebarState {
            lsp_servers: vec![ready_lsp("rust-analyzer")],
            mcp_servers: vec![SidebarMcp {
                server_id: "github".to_string(),
                tool_count: 3,
                resource_count: 0,
                prompt_count: 0,
                connected: true,
                error: None,
            }],
            ..Default::default()
        };
        let text = render_sidebar_to_string(50, 25, &state);
        let lsp_pos = text.find("LSP").expect("LSP header not found");
        let mcp_pos = text.find("MCP").expect("MCP header not found");
        assert!(lsp_pos < mcp_pos, "LSP should render above MCP");
    }

    #[test]
    fn buffer_sidebar_mcp_connected_tools_only() {
        let state = SidebarState {
            mcp_servers: vec![SidebarMcp {
                server_id: "simple".to_string(),
                tool_count: 0,
                resource_count: 0,
                prompt_count: 0,
                connected: true,
                error: None,
            }],
            ..Default::default()
        };
        let text = render_sidebar_to_string(50, 20, &state);
        // Connected with zero tools/resources should just show server_id
        assert!(text.contains("simple"), "should show server id");
        assert!(!text.contains("0T"), "should not show tool count when zero");
    }

    #[test]
    fn max_branch_display_values() {
        assert_eq!(max_branch_display(36), 34);
        assert_eq!(max_branch_display(44), 42);
        assert_eq!(max_branch_display(12), 10);
        assert_eq!(max_branch_display(0), 0);
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
        // Verify priority abbreviation doesn't appear on the task ID line
        let id_line = text.lines().find(|l| l.contains("task-nopr0001")).unwrap();
        assert!(
            !id_line.contains(" hi"),
            "should not show priority abbreviation on ID line"
        );
    }

    // -- Changes sorting test --

    #[test]
    fn buffer_sidebar_changes_sorted_alphabetically() {
        let mut state = SidebarState {
            git_branch: Some("main".to_string()),
            ..Default::default()
        };
        // Insert in non-alphabetical order
        state.record_file_change("src/zebra.rs".into(), 1, 0);
        state.record_file_change("src/alpha.rs".into(), 2, 0);
        state.record_file_change("src/middle.rs".into(), 3, 0);
        let text = render_sidebar_to_string(40, 20, &state);
        let alpha_pos = text.find("alpha.rs").expect("alpha.rs not found");
        let middle_pos = text.find("middle.rs").expect("middle.rs not found");
        let zebra_pos = text.find("zebra.rs").expect("zebra.rs not found");
        assert!(
            alpha_pos < middle_pos && middle_pos < zebra_pos,
            "changes should be sorted alphabetically, got:\n{text}"
        );
    }

    // -- Changes path shortening test --

    #[test]
    fn buffer_sidebar_changes_shortens_long_paths() {
        let mut state = SidebarState {
            git_branch: Some("main".to_string()),
            ..Default::default()
        };
        state.record_file_change(
            "src/components/dialogs/widgets/very_long_filename.rs".into(),
            5,
            2,
        );
        // With a 40-col sidebar, the path should be shortened
        let text = render_sidebar_to_string(40, 20, &state);
        assert!(
            text.contains("s/c/d/w/very_long_filename.rs"),
            "long path should be shortened, got:\n{text}"
        );
    }
}
