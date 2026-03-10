use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
};

use crate::ui::theme::Theme;

use super::state::{DataState, FilterField, SortColumn, View};

/// Render the appropriate view based on current state.
pub fn render(frame: &mut Frame, state: &DataState, theme: &Theme) {
    let area = frame.area();

    match state.view {
        View::SessionList => render_session_list(frame, area, state, theme),
        View::SessionDetail => render_session_detail(frame, area, state, theme),
    }
}

/// Format a token count for display (e.g., 12400 → "12.4k", 1200000 → "1.2M").
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format cost for display (None → "--", Some → "$0.08").
fn format_cost(cost: Option<f64>) -> String {
    match cost {
        Some(c) if c >= 1.0 => format!("${:.2}", c),
        Some(c) => format!("${:.3}", c),
        None => "--".to_string(),
    }
}

/// Format duration in ms to human-readable (e.g., "1.2s", "45.3s", "2m 15s").
fn format_duration(ms: u64) -> String {
    let secs = ms as f64 / 1000.0;
    if secs < 60.0 {
        format!("{:.1}s", secs)
    } else {
        let mins = (secs / 60.0).floor() as u64;
        let remaining = (secs % 60.0).round() as u64;
        format!("{}m {}s", mins, remaining)
    }
}

/// Extract a short date from an RFC3339 string (e.g., "2026-03-10T14:..." → "2026-03-10").
fn short_date(rfc3339: &str) -> &str {
    rfc3339.get(..10).unwrap_or(rfc3339)
}

/// Extract a short time from an RFC3339 string (e.g., "...T14:32:01..." → "14:32:01").
fn short_time(rfc3339: &str) -> &str {
    // RFC3339: "2026-03-10T14:32:01.000Z" — time starts at index 11
    rfc3339
        .get(11..19)
        .unwrap_or(rfc3339.get(11..).unwrap_or(""))
}

/// Truncate a string to fit in `max_width` chars, appending "…" if truncated.
fn truncate(s: &str, max_width: usize) -> String {
    if s.chars().count() <= max_width {
        s.to_string()
    } else if max_width <= 1 {
        "…".to_string()
    } else {
        let truncated: String = s.chars().take(max_width - 1).collect();
        format!("{truncated}…")
    }
}

// ── Session List View ───────────────────────────────────────

fn render_session_list(frame: &mut Frame, area: Rect, state: &DataState, theme: &Theme) {
    // Layout: [filter_bar?] [table] [stats_footer] [help_line]
    let filter_height = if state.filter_active { 1 } else { 0 };
    let chunks = Layout::vertical([
        Constraint::Length(filter_height),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    if state.filter_active {
        render_filter_bar(frame, chunks[0], state, theme);
    }

    render_session_table(frame, chunks[1], state, theme);
    render_stats_footer(frame, chunks[2], state, theme);
    render_help_line(frame, chunks[3], state, theme);
}

fn render_filter_bar(frame: &mut Frame, area: Rect, state: &DataState, theme: &Theme) {
    let project_label = match state.filter_project_idx {
        Some(idx) => state
            .projects
            .get(idx)
            .map(|p| p.display_name.as_str())
            .unwrap_or("All"),
        None => "All",
    };
    let model_label = match state.filter_model_idx {
        Some(idx) => state.models.get(idx).map(|s| s.as_str()).unwrap_or("All"),
        None => "All",
    };

    let project_style = if state.filter_field == FilterField::Project {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg)
    };
    let model_style = if state.filter_field == FilterField::Model {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg)
    };

    let line = Line::from(vec![
        Span::styled(" Filter: ", Style::default().fg(theme.dim)),
        Span::styled("Project: ", Style::default().fg(theme.dim)),
        Span::styled(format!("[{project_label}]"), project_style),
        Span::styled("  Model: ", Style::default().fg(theme.dim)),
        Span::styled(format!("[{model_label}]"), model_style),
        Span::styled(
            "  Tab cycle  ←→ pick  Enter apply  Esc cancel",
            Style::default().fg(theme.dim),
        ),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

fn render_session_table(frame: &mut Frame, area: Rect, state: &DataState, theme: &Theme) {
    if state.sessions.is_empty() {
        let msg = Paragraph::new("  No usage data yet. Start a conversation to record usage.")
            .style(Style::default().fg(theme.dim))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border))
                    .title(Span::styled(
                        " steve data ",
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    )),
            );
        frame.render_widget(msg, area);
        return;
    }

    // Build header with sort indicator
    let header_cells: Vec<Span> = SortColumn::ALL
        .iter()
        .map(|col| {
            let label = col.label();
            let arrow = if *col == state.sort_column {
                if state.sort_ascending {
                    " ▲"
                } else {
                    " ▼"
                }
            } else {
                ""
            };
            let style = if *col == state.sort_column {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.dim)
            };
            Span::styled(format!("{label}{arrow}"), style)
        })
        .collect();

    let header = Row::new(header_cells).height(1);

    // Visible window size for scrolling
    let table_height = area.height.saturating_sub(3) as usize; // borders + header
    let offset = state.list_offset;

    let rows: Vec<Row> = state
        .sessions
        .iter()
        .enumerate()
        .skip(offset)
        .take(table_height.max(1))
        .map(|(i, s)| {
            let is_selected = i == state.selected_row;
            let style = if is_selected {
                Style::default().fg(theme.fg).bg(theme.selection_bg)
            } else {
                Style::default().fg(theme.fg)
            };
            let pointer = if is_selected { "▸" } else { " " };

            // Compute available width for title (area minus other columns and padding)
            let title_max = (area.width as usize).saturating_sub(55);

            Row::new(vec![
                format!("{} {}", pointer, short_date(&s.created_at)),
                truncate(&s.title, title_max.max(10)),
                truncate(&s.model_ref, 18),
                format_tokens(s.total_tokens),
                format_cost(s.total_cost),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(13), // Date
        Constraint::Min(15),    // Title (fills remaining)
        Constraint::Length(20), // Model
        Constraint::Length(8),  // Tokens
        Constraint::Length(8),  // Cost
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border))
                .title(Span::styled(
                    " steve data ",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .column_spacing(1);

    frame.render_widget(table, area);
}

fn render_stats_footer(frame: &mut Frame, area: Rect, state: &DataState, theme: &Theme) {
    let stats = &state.stats;
    let line = Line::from(vec![
        Span::styled(
            format!(
                " {} sessions · {} calls · {} tokens · {}",
                stats.session_count,
                stats.call_count,
                format_tokens(stats.total_tokens),
                format_cost(Some(stats.total_cost)),
            ),
            Style::default().fg(theme.dim),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_help_line(frame: &mut Frame, area: Rect, state: &DataState, theme: &Theme) {
    let help = match state.view {
        View::SessionList => {
            " ↑↓/j/k navigate  Enter drill-down  Tab sort  r reverse  f filter  q quit"
        }
        View::SessionDetail => " ↑↓/j/k navigate  Esc/Backspace back  q quit",
    };
    let line = Line::from(Span::styled(help, Style::default().fg(theme.dim)));
    frame.render_widget(Paragraph::new(line), area);
}

// ── Session Detail View ─────────────────────────────────────

fn render_session_detail(frame: &mut Frame, area: Rect, state: &DataState, theme: &Theme) {
    let chunks = Layout::vertical([
        Constraint::Length(2), // Session info header
        Constraint::Min(3),    // API calls table
        Constraint::Length(1), // Help line
    ])
    .split(area);

    // Session header
    let session_total_tokens: u64 = state.detail_calls.iter().map(|c| c.total_tokens as u64).sum();
    let session_total_cost: f64 = state
        .detail_calls
        .iter()
        .filter_map(|c| c.cost)
        .sum();
    let cost_str = if state.detail_calls.iter().any(|c| c.cost.is_some()) {
        format_cost(Some(session_total_cost))
    } else {
        "--".to_string()
    };

    let header_line = Line::from(vec![
        Span::styled(" Session: ", Style::default().fg(theme.dim)),
        Span::styled(
            &state.detail_session_title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  {}  {}  {} tokens  {}  {} calls",
                state.detail_session_model,
                short_date(&state.detail_session_date),
                format_tokens(session_total_tokens),
                cost_str,
                state.detail_calls.len(),
            ),
            Style::default().fg(theme.dim),
        ),
    ]);
    frame.render_widget(Paragraph::new(vec![Line::raw(""), header_line]), chunks[0]);

    // API calls table
    if state.detail_calls.is_empty() {
        let msg = Paragraph::new("  No API calls recorded for this session.")
            .style(Style::default().fg(theme.dim))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            );
        frame.render_widget(msg, chunks[1]);
    } else {
        let header = Row::new(vec![
            Span::styled("#", Style::default().fg(theme.dim)),
            Span::styled("Time", Style::default().fg(theme.dim)),
            Span::styled("Iter", Style::default().fg(theme.dim)),
            Span::styled("Prompt", Style::default().fg(theme.dim)),
            Span::styled("Compl", Style::default().fg(theme.dim)),
            Span::styled("Total", Style::default().fg(theme.dim)),
            Span::styled("Cost", Style::default().fg(theme.dim)),
            Span::styled("Duration", Style::default().fg(theme.dim)),
        ])
        .height(1);

        let table_height = chunks[1].height.saturating_sub(3) as usize;
        let offset = state.detail_offset;

        let rows: Vec<Row> = state
            .detail_calls
            .iter()
            .enumerate()
            .skip(offset)
            .take(table_height.max(1))
            .map(|(i, c)| {
                let is_selected = i == state.detail_selected;
                let style = if is_selected {
                    Style::default().fg(theme.fg).bg(theme.selection_bg)
                } else {
                    Style::default().fg(theme.fg)
                };

                Row::new(vec![
                    format!("{}", i + 1),
                    short_time(&c.timestamp).to_string(),
                    c.iteration.to_string(),
                    format_tokens(c.prompt_tokens as u64),
                    format_tokens(c.completion_tokens as u64),
                    format_tokens(c.total_tokens as u64),
                    format_cost(c.cost),
                    format_duration(c.duration_ms),
                ])
                .style(style)
            })
            .collect();

        let widths = [
            Constraint::Length(4),  // #
            Constraint::Length(10), // Time
            Constraint::Length(5),  // Iter
            Constraint::Length(8),  // Prompt
            Constraint::Length(8),  // Compl
            Constraint::Length(8),  // Total
            Constraint::Length(8),  // Cost
            Constraint::Length(10), // Duration
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            )
            .column_spacing(1);

        frame.render_widget(table, chunks[1]);
    }

    render_help_line(frame, chunks[2], state, theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tokens_sub_thousand() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(1000), "1.0k");
        assert_eq!(format_tokens(12400), "12.4k");
        assert_eq!(format_tokens(999_999), "1000.0k");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn format_cost_none_shows_dashes() {
        assert_eq!(format_cost(None), "--");
    }

    #[test]
    fn format_cost_sub_dollar() {
        assert_eq!(format_cost(Some(0.08)), "$0.080");
        assert_eq!(format_cost(Some(0.005)), "$0.005");
    }

    #[test]
    fn format_cost_over_dollar() {
        assert_eq!(format_cost(Some(1.12)), "$1.12");
        assert_eq!(format_cost(Some(10.5)), "$10.50");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(1200), "1.2s");
        assert_eq!(format_duration(500), "0.5s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(135_000), "2m 15s");
    }

    #[test]
    fn short_date_extracts_date() {
        assert_eq!(short_date("2026-03-10T14:32:01.000Z"), "2026-03-10");
    }

    #[test]
    fn short_time_extracts_time() {
        assert_eq!(short_time("2026-03-10T14:32:01.000Z"), "14:32:01");
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_adds_ellipsis() {
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn truncate_width_1() {
        assert_eq!(truncate("hello", 1), "…");
    }
}
