use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use std::time::Duration;

use super::{
    markdown::{MarkdownLine, is_table_row, render_markdown_line, render_table},
    message_block::{
        AssistantPart, CodeFence, DiffContent, DiffLine, MessageBlock, ToolGroup, ToolGroupStatus,
    },
    primitives,
    selection::{ContentMap, ContentPos, SelectionState},
    status_line::format_elapsed_compact,
    syntax,
    theme::Theme,
};
use crate::tool::{IntentCategory, ToolName, ToolVisualCategory};

/// State for the scrollable message area.
///
/// Coordinate system: `scroll_offset = 0` means top of content.
/// Auto-scroll sets `scroll_offset = max_scroll` (bottom of content).
/// This aligns with ratatui's `Paragraph::scroll((row, 0))` API.
pub struct MessageAreaState {
    /// Current scroll position (0 = top of content).
    pub scroll_offset: u16,
    /// Whether to automatically scroll to follow new content.
    pub auto_scroll: bool,
    /// Total content height from last render (used for clamping).
    content_height: u16,
    /// Visible area height from last render.
    visible_height: u16,
    /// Content map built during last render (for coordinate mapping).
    pub content_map: Option<ContentMap>,
}

impl Default for MessageAreaState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            auto_scroll: true,
            content_height: 0,
            visible_height: 0,
            content_map: None,
        }
    }
}

impl MessageAreaState {
    /// Maximum scroll offset (0 if content fits in view).
    pub fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.visible_height)
    }

    /// Scroll toward older content (up). Disables auto-scroll.
    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        self.auto_scroll = false;
    }

    /// Scroll toward newer content (down). Re-enables auto-scroll at bottom.
    pub fn scroll_down(&mut self, amount: u16) {
        let max = self.max_scroll();
        self.scroll_offset = self.scroll_offset.saturating_add(amount).min(max);
        if self.scroll_offset >= max {
            self.auto_scroll = true;
        }
    }

    /// Visible area height (for page-size scrolling).
    pub fn visible_height(&self) -> u16 {
        self.visible_height
    }

    /// Jump to the bottom (newest content). Re-enables auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll();
        self.auto_scroll = true;
    }

    /// Update dimensions from render. If auto-scroll, jump to bottom.
    /// Clamp offset to valid range.
    pub fn update_dimensions(&mut self, content_height: u16, visible_height: u16) {
        self.content_height = content_height;
        self.visible_height = visible_height;
        let max = self.max_scroll();
        if self.auto_scroll {
            self.scroll_offset = max;
        } else {
            self.scroll_offset = self.scroll_offset.min(max);
        }
    }
}

/// Width of the activity rail gutter in columns: marker (1) + space (1) + separator (1).
const GUTTER_WIDTH: usize = 3;

/// What to show in the left gutter for a given line.
#[derive(Debug, Clone, Copy)]
enum GutterMark {
    /// Empty gutter — text, user, system, error, blanks.
    Empty,
    /// Tool header line — shows the tool's marker character.
    ToolMarker(ToolName),
    /// Continuation line (expanded diff/output) — shows dim pipe.
    Continuation(ToolName),
    /// Intent indicator line — shows dim dash.
    Intent,
}

/// Resolve the UI color for a tool name via `ToolVisualCategory`.
fn tool_color(name: ToolName, theme: &Theme) -> ratatui::style::Color {
    match name.visual_category() {
        ToolVisualCategory::Read => theme.tool_read,
        ToolVisualCategory::Write => theme.tool_write,
        ToolVisualCategory::Accent => theme.accent,
    }
}

/// Return a guaranteed-1-column marker character for the gutter.
/// Delegates to `ToolName::gutter_char()`.
fn gutter_marker(name: ToolName) -> &'static str {
    name.gutter_char()
}

/// Build gutter spans for a line based on its mark type.
fn gutter_spans(mark: GutterMark, theme: &Theme) -> Vec<Span<'static>> {
    match mark {
        GutterMark::Empty => vec![Span::raw("   ")],
        GutterMark::ToolMarker(name) => vec![
            Span::styled(
                gutter_marker(name).to_string(),
                Style::default().fg(tool_color(name, theme)),
            ),
            Span::styled(" \u{2502}", Style::default().fg(theme.dim)),
        ],
        GutterMark::Continuation(_) => vec![Span::styled(
            "\u{2502} \u{2502}",
            Style::default().fg(theme.dim),
        )],
        GutterMark::Intent => vec![Span::styled(
            "\u{2500} \u{2502}",
            Style::default().fg(theme.dim),
        )],
    }
}

/// Prepend gutter spans to a line, preserving its existing style.
fn prepend_gutter<'a>(line: Line<'a>, mark: GutterMark, theme: &Theme) -> Line<'a> {
    let gutter = gutter_spans(mark, theme);
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(gutter.len() + line.spans.len());
    for s in gutter {
        spans.push(s);
    }
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

/// Wrapper around `Vec<Line>` that auto-prepends gutter marks to every line
/// and tracks parallel plain text for ContentMap building.
struct GutteredLines<'a> {
    lines: Vec<Line<'a>>,
    /// Plain text for each line (gutter-stripped), parallel to `lines`.
    texts: Vec<String>,
    theme: &'a Theme,
}

impl<'a> GutteredLines<'a> {
    fn new(theme: &'a Theme) -> Self {
        Self {
            lines: Vec::new(),
            texts: Vec::new(),
            theme,
        }
    }

    fn push(&mut self, line: Line<'a>, mark: GutterMark) {
        let plain = extract_plain_text(&line);
        self.lines.push(prepend_gutter(line, mark, self.theme));
        self.texts.push(plain);
    }

    /// Push a line with an explicit plain text override.
    /// Use when the line contains decoration spans (e.g. `│ `) that shouldn't
    /// appear in clipboard text.
    fn push_with_text(&mut self, line: Line<'a>, mark: GutterMark, plain: String) {
        self.lines.push(prepend_gutter(line, mark, self.theme));
        self.texts.push(plain);
    }

    /// Extend with lines from a helper function, applying the same mark to all.
    fn extend(&mut self, new_lines: Vec<Line<'a>>, mark: GutterMark) {
        for line in new_lines {
            let plain = extract_plain_text(&line);
            self.lines.push(prepend_gutter(line, mark, self.theme));
            self.texts.push(plain);
        }
    }

    fn into_lines_and_texts(self) -> (Vec<Line<'a>>, Vec<String>) {
        (self.lines, self.texts)
    }
}

/// Extract plain text content from a Line's spans (before gutter prepend).
fn extract_plain_text(line: &Line<'_>) -> String {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(&span.content);
    }
    text
}

/// Render structured message blocks into the given area.
// Structural — these args are all needed
#[allow(clippy::too_many_arguments)]
pub fn render_message_blocks(
    frame: &mut Frame,
    area: Rect,
    messages: &[MessageBlock],
    state: &mut MessageAreaState,
    theme: &Theme,
    activity: Option<(char, String, bool, Option<Duration>)>,
    context_pct: u8,
    selection: &SelectionState,
) {
    let mut glines = GutteredLines::new(theme);
    let available_width = area.width.max(1) as usize;
    let content_width = available_width.saturating_sub(GUTTER_WIDTH);
    let visible_height = area.height;

    // 3A: Empty state welcome message
    if messages.is_empty() && activity.is_none() {
        let welcome_y = (visible_height as usize * 40 / 100).max(1);
        // Pad blank lines to place content slightly above center
        for _ in 0..welcome_y {
            glines.push(Line::from(""), GutterMark::Empty);
        }

        // "steve" in accent
        let steve_label = "steve";
        let steve_pad = content_width.saturating_sub(steve_label.len()) / 2;
        glines.push(
            Line::from(vec![
                Span::raw(" ".repeat(steve_pad)),
                Span::styled(
                    steve_label,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            GutterMark::Empty,
        );

        // Subtitle lines in dim
        let lines_text = [
            "Type a message to get started.",
            "Tab to toggle Build/Plan mode.",
        ];
        for subtitle in lines_text {
            let pad = content_width.saturating_sub(subtitle.len()) / 2;
            glines.push(
                Line::from(vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(subtitle, Style::default().fg(theme.dim)),
                ]),
                GutterMark::Empty,
            );
        }

        // Render and return early — skip the message loop
        let (lines, texts) = glines.into_lines_and_texts();
        let content_map = ContentMap::build(texts, available_width);
        state.content_map = Some(content_map);
        let content_height = lines.len() as u16;
        state.update_dimensions(content_height, visible_height);

        let block = Block::default()
            .borders(Borders::NONE)
            .style(Style::default().fg(theme.fg));
        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((state.scroll_offset, 0));
        frame.render_widget(paragraph, area);
        return;
    }

    for msg in messages.iter() {
        match msg {
            MessageBlock::User { text } => {
                for text_line in text.lines() {
                    let mut spans = vec![Span::styled(
                        "│ ",
                        Style::default()
                            .fg(theme.user_msg)
                            .bg(theme.user_msg_bg)
                            .add_modifier(Modifier::BOLD),
                    )];
                    spans.extend(style_file_refs(text_line, theme));
                    // Use push_with_text to exclude the "│ " decoration from clipboard text
                    let line = Line::from(spans).style(Style::default().bg(theme.user_msg_bg));
                    glines.push_with_text(line, GutterMark::Empty, text_line.to_string());
                }
            }

            MessageBlock::Assistant { thinking, parts } => {
                // Thinking block (collapsed by default)
                if let Some(t) = thinking {
                    if t.expanded {
                        glines.push(
                            Line::from(Span::styled(
                                format!("\u{25bc} Thinking ({} tokens)", t.token_count),
                                Style::default()
                                    .fg(theme.reasoning)
                                    .add_modifier(Modifier::ITALIC),
                            )),
                            GutterMark::Empty,
                        );
                        for content_line in t.content.lines() {
                            glines.push(
                                Line::from(Span::styled(
                                    format!("  {content_line}"),
                                    Style::default().fg(theme.reasoning),
                                )),
                                GutterMark::Empty,
                            );
                        }
                    } else {
                        glines.push(
                            Line::from(Span::styled(
                                format!("\u{25b6} Thinking ({} tokens)", t.token_count),
                                Style::default()
                                    .fg(theme.reasoning)
                                    .add_modifier(Modifier::ITALIC),
                            )),
                            GutterMark::Empty,
                        );
                    }
                }

                // Parts in chronological order.
                // Track last-emitted intent to suppress repeated labels for
                // consecutive same-category tool groups (e.g. 3 reads in a row).
                // Text between groups resets tracking so the label reappears.
                let mut last_intent: Option<IntentCategory> = None;
                for part in parts.iter() {
                    match part {
                        AssistantPart::Text(text) => {
                            let md_lines = render_text_with_code_blocks(text, theme, content_width);
                            for ml in md_lines {
                                glines.push_with_text(ml.styled, GutterMark::Empty, ml.plain);
                            }
                            last_intent = None;
                        }
                        AssistantPart::ToolGroup(group) => {
                            // Intent indicator — suppressed if same as previous group
                            if let Some(category) = infer_group_intent(group) {
                                if last_intent != Some(category) {
                                    glines.push(
                                        render_intent_line(category, content_width, theme),
                                        GutterMark::Intent,
                                    );
                                }
                                last_intent = Some(category);
                            } else {
                                // Asking-only groups have no label — reset tracking so the
                                // next labeled group isn't incorrectly suppressed.
                                last_intent = None;
                            }
                            for call in &group.calls {
                                let status_indicator = match (&group.status, &call.result_summary) {
                                    (_, Some(_)) if call.expanded => "\u{25bc}",
                                    (_, Some(_)) => "\u{25b6}",
                                    _ => "\u{2819}",
                                };

                                let result_part = match &call.result_summary {
                                    Some(summary) => format!(" \u{2192} {summary}"),
                                    None => match &group.status {
                                        ToolGroupStatus::Preparing => " preparing...".to_string(),
                                        ToolGroupStatus::Running { .. } => {
                                            " running...".to_string()
                                        }
                                        ToolGroupStatus::Complete => String::new(),
                                    },
                                };

                                let color = if call.is_error {
                                    theme.error
                                } else {
                                    tool_color(call.tool_name, theme)
                                };

                                glines.push(
                                    Line::from(vec![
                                        Span::styled(
                                            format!("{status_indicator} "),
                                            Style::default().fg(color),
                                        ),
                                        Span::styled(
                                            call.tool_name.to_string(),
                                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                                        ),
                                        Span::styled(
                                            format!(" {}{}", call.args_summary, result_part),
                                            Style::default().fg(color),
                                        ),
                                    ]),
                                    GutterMark::ToolMarker(call.tool_name),
                                );

                                // Sub-agent live progress — show what tool the agent is calling
                                if let Some(progress) = &call.agent_progress {
                                    let progress_text =
                                        if let Some(ref result) = progress.result_summary {
                                            format!(
                                                "    {} {} \u{2192} {result}",
                                                progress.tool_name, progress.args_summary
                                            )
                                        } else {
                                            format!(
                                                "    {} {} ...",
                                                progress.tool_name, progress.args_summary
                                            )
                                        };
                                    glines.push(
                                        Line::from(Span::styled(
                                            progress_text,
                                            Style::default().fg(theme.dim),
                                        )),
                                        GutterMark::Continuation(call.tool_name),
                                    );
                                }

                                // Expanded output — diff content or raw output fallback
                                if call.expanded {
                                    if let Some(diff) = &call.diff_content {
                                        let mut diff_output: Vec<Line> = Vec::new();
                                        render_diff_lines(
                                            &mut diff_output,
                                            diff,
                                            call.result_summary.as_deref(),
                                            theme,
                                            context_pct,
                                            content_width,
                                        );
                                        glines.extend(
                                            diff_output,
                                            GutterMark::Continuation(call.tool_name),
                                        );
                                    } else if let Some(output) = &call.full_output {
                                        for output_line in output.lines() {
                                            glines.push(
                                                Line::from(Span::styled(
                                                    format!("  {output_line}"),
                                                    Style::default().fg(theme.dim),
                                                )),
                                                GutterMark::Continuation(call.tool_name),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            MessageBlock::System { text } => {
                for (i, text_line) in text.lines().enumerate() {
                    if i == 0 {
                        // Rule-style: ── Message text ──────────────
                        let prefix = format!("\u{2500}\u{2500} {text_line} ");
                        let prefix_chars = prefix.chars().count();
                        let dash_count = content_width.saturating_sub(prefix_chars);
                        let dashes = "\u{2500}".repeat(dash_count);
                        glines.push(
                            Line::from(Span::styled(
                                format!("{prefix}{dashes}"),
                                Style::default().fg(theme.system_msg),
                            )),
                            GutterMark::Empty,
                        );
                    } else {
                        glines.push(
                            Line::from(Span::styled(
                                format!("   {text_line}"),
                                Style::default()
                                    .fg(theme.system_msg)
                                    .add_modifier(Modifier::ITALIC),
                            )),
                            GutterMark::Empty,
                        );
                    }
                }
            }

            MessageBlock::Error { text } => {
                for text_line in text.lines() {
                    glines.push(
                        Line::from(Span::styled(
                            text_line.to_string(),
                            Style::default().fg(theme.error),
                        )),
                        GutterMark::Empty,
                    );
                }
            }

            MessageBlock::Permission {
                tool_name,
                args_summary,
                diff_content,
            } => {
                // Top rule
                glines.push(
                    primitives::horizontal_rule(content_width, theme.permission),
                    GutterMark::Empty,
                );
                // Prompt line
                glines.push(
                    Line::from(vec![
                        Span::styled(
                            "\u{26a0} Allow ",
                            Style::default()
                                .fg(theme.permission)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            tool_name.to_string(),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(": {args_summary}?"),
                            Style::default()
                                .fg(theme.permission)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    GutterMark::Empty,
                );
                // Inline diff preview if available
                if let Some(diff) = diff_content {
                    let mut diff_output: Vec<Line> = Vec::new();
                    render_diff_lines(
                        &mut diff_output,
                        diff,
                        None,
                        theme,
                        context_pct,
                        content_width,
                    );
                    glines.extend(diff_output, GutterMark::Empty);
                }
                // Options line with highlighted key letters
                glines.push(
                    Line::from(vec![
                        Span::raw("  ["),
                        Span::styled(
                            "y",
                            Style::default()
                                .fg(theme.success)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("]es / ["),
                        Span::styled(
                            "n",
                            Style::default()
                                .fg(theme.error)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("]o / ["),
                        Span::styled(
                            "a",
                            Style::default()
                                .fg(theme.permission)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("]lways"),
                    ]),
                    GutterMark::Empty,
                );
                // Bottom rule
                glines.push(
                    primitives::horizontal_rule(content_width, theme.permission),
                    GutterMark::Empty,
                );
            }

            MessageBlock::Question {
                question,
                options,
                selected,
                free_text,
                answered,
            } => {
                // Top rule
                glines.push(
                    primitives::horizontal_rule(content_width, theme.question),
                    GutterMark::Empty,
                );
                // Question line with mode badge
                let mode_badge = if answered.is_some() {
                    "" // No badge for answered questions
                } else if selected.is_none() {
                    " [typing]"
                } else {
                    " [selecting]"
                };
                glines.push(
                    Line::from(vec![
                        Span::styled(
                            "? ",
                            Style::default()
                                .fg(theme.question)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            question.to_string(),
                            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(mode_badge.to_string(), Style::default().fg(theme.question)),
                    ]),
                    GutterMark::Empty,
                );

                if let Some(answer) = answered {
                    // Answered state — show the answer
                    glines.push(
                        Line::from(Span::styled(
                            format!("  \u{2192} {answer}"),
                            Style::default().fg(theme.success),
                        )),
                        GutterMark::Empty,
                    );
                } else {
                    // Active state — show options or free-text input
                    let in_free_text_mode = selected.is_none();
                    for (i, option) in options.iter().enumerate() {
                        let is_selected = *selected == Some(i);
                        let prefix = if is_selected { "  \u{25b8} " } else { "    " };
                        let label = format!("{prefix}{}. {option}", i + 1);
                        let style = if is_selected {
                            Style::default()
                                .fg(theme.question)
                                .add_modifier(Modifier::BOLD)
                        } else if in_free_text_mode {
                            // Dim options when in free-text mode
                            Style::default().fg(theme.dim)
                        } else {
                            Style::default().fg(theme.fg)
                        };
                        glines.push(Line::from(Span::styled(label, style)), GutterMark::Empty);
                    }

                    // Free-text input (when no options or Tab toggled to free-text)
                    if selected.is_none() {
                        glines.push(
                            Line::from(Span::styled(
                                format!("  > {free_text}\u{258f}"),
                                Style::default().fg(theme.fg),
                            )),
                            GutterMark::Empty,
                        );
                    }

                    // Help line
                    let mut help_spans = Vec::new();
                    if !options.is_empty() {
                        help_spans.push(Span::raw("  ["));
                        help_spans.push(Span::styled(
                            "1-9",
                            Style::default()
                                .fg(theme.question)
                                .add_modifier(Modifier::BOLD),
                        ));
                        help_spans.push(Span::raw("] select  ["));
                        help_spans.push(Span::styled(
                            "Tab",
                            Style::default()
                                .fg(theme.question)
                                .add_modifier(Modifier::BOLD),
                        ));
                        let tab_label = if selected.is_some() {
                            "] free text  "
                        } else {
                            "] options  "
                        };
                        help_spans.push(Span::raw(tab_label));
                    } else {
                        help_spans.push(Span::raw("  "));
                    }
                    help_spans.push(Span::raw("["));
                    help_spans.push(Span::styled(
                        "Enter",
                        Style::default()
                            .fg(theme.success)
                            .add_modifier(Modifier::BOLD),
                    ));
                    help_spans.push(Span::raw("] confirm  ["));
                    help_spans.push(Span::styled(
                        "Esc",
                        Style::default()
                            .fg(theme.error)
                            .add_modifier(Modifier::BOLD),
                    ));
                    help_spans.push(Span::raw("] skip"));
                    glines.push(Line::from(help_spans), GutterMark::Empty);
                }

                // Bottom rule
                glines.push(
                    primitives::horizontal_rule(content_width, theme.question),
                    GutterMark::Empty,
                );
            }
        }

        // Blank line between messages
        glines.push(Line::from(""), GutterMark::Empty);
    }

    // Inline activity spinner (replaces the old "..." and status bar spinner)
    if let Some((spinner, text, has_pending, activity_elapsed)) = activity {
        let mut activity_text = format!("{spinner} {text}");
        if let Some(elapsed) = activity_elapsed {
            activity_text.push(' ');
            activity_text.push_str(&format_elapsed_compact(elapsed));
        }
        let mut spans = vec![Span::styled(
            activity_text,
            Style::default().fg(theme.accent),
        )];
        if has_pending {
            spans.push(Span::styled(
                "  (message queued)",
                Style::default().fg(theme.dim),
            ));
        }
        glines.push(Line::from(spans), GutterMark::Empty);
        glines.push(Line::from(""), GutterMark::Empty);
    }

    let (mut lines, texts) = glines.into_lines_and_texts();

    // Build content map for coordinate mapping (used by mouse selection)
    let content_map = ContentMap::build(texts, available_width);
    state.content_map = Some(content_map);

    // Apply selection highlighting
    if let Some((sel_start, sel_end)) = selection.ordered_range() {
        apply_selection_highlight(&mut lines, &sel_start, &sel_end, available_width, theme);
    }

    // Compute content height with wrapping
    let content_height_u32: u32 = lines
        .iter()
        .map(|line| {
            let line_width: usize = line.width();
            if line_width == 0 {
                1u32
            } else {
                line_width.div_ceil(available_width) as u32
            }
        })
        .sum();
    let content_height = content_height_u32.min(u16::MAX as u32) as u16;
    let visible_height = area.height; // Borders::NONE — full area is visible

    state.update_dimensions(content_height, visible_height);

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().fg(theme.fg));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));

    frame.render_widget(paragraph, area);

    // Scroll position indicator overlay
    if !state.auto_scroll && state.scroll_offset > 0 {
        let lines_above = state.scroll_offset;
        let indicator = format!(" \u{2191} {} lines above ", lines_above);
        let ind_width = indicator.chars().count() as u16;
        if area.width >= ind_width + 2 {
            let ind_area = Rect::new(area.x + area.width - ind_width - 1, area.y, ind_width, 1);
            let ind_widget = Paragraph::new(Line::from(Span::styled(
                indicator,
                Style::default().fg(theme.dim),
            )));
            frame.render_widget(ind_widget, ind_area);
        }
    }

    // "Copied!" flash overlay
    if let Some(flash_time) = selection.copied_flash
        && flash_time.elapsed().as_millis() < 1000
    {
        let flash_text = " Copied! ";
        let flash_width = flash_text.len() as u16;
        if area.width >= flash_width + 2 {
            let flash_area = Rect::new(
                area.x + area.width - flash_width - 1,
                area.y,
                flash_width,
                1,
            );
            let flash = Paragraph::new(Line::from(Span::styled(
                flash_text,
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            )));
            frame.render_widget(flash, flash_area);
        }
    }
}

/// Apply selection highlighting to lines between `start` and `end` content positions.
///
/// Modifies span background colors on affected lines. The gutter occupies the first
/// `GUTTER_WIDTH` characters of each line — selection only highlights content spans.
fn apply_selection_highlight(
    lines: &mut [Line<'_>],
    start: &ContentPos,
    end: &ContentPos,
    _available_width: usize,
    theme: &Theme,
) {
    for line_idx in start.line..=end.line.min(lines.len().saturating_sub(1)) {
        if line_idx >= lines.len() {
            break;
        }

        // Determine the character range to highlight within this line's content (after gutter)
        let line_start = if line_idx == start.line {
            start.char_offset
        } else {
            0
        };
        let line_end = if line_idx == end.line {
            end.char_offset
        } else {
            // Select to end of line — use a large number, clamping happens below
            usize::MAX
        };

        if line_start >= line_end && line_idx == start.line && line_idx == end.line {
            continue;
        }

        // Build new spans list, splitting at selection boundaries
        let mut char_pos: usize = 0;
        let mut in_gutter = true;
        let mut gutter_chars = 0;
        let mut new_spans: Vec<Span<'_>> = Vec::new();

        let line = &mut lines[line_idx];
        for span in line.spans.drain(..) {
            let span_char_count = span.content.chars().count();

            if in_gutter {
                gutter_chars += span_char_count;
                if gutter_chars >= GUTTER_WIDTH {
                    in_gutter = false;
                }
                new_spans.push(span);
                continue;
            }

            let span_start = char_pos;
            let span_end = char_pos + span_char_count;

            if span_start >= line_end || span_end <= line_start {
                // No overlap — keep original
                new_spans.push(span);
            } else {
                // Overlap — split the span at selection boundaries
                let sel_start_in_span = line_start.saturating_sub(span_start);
                let sel_end_in_span = (line_end - span_start).min(span_char_count);
                let chars: Vec<char> = span.content.chars().collect();

                // Before selection
                if sel_start_in_span > 0 {
                    let before: String = chars[..sel_start_in_span].iter().collect();
                    new_spans.push(Span::styled(before, span.style));
                }
                // Selected portion
                let selected: String = chars[sel_start_in_span..sel_end_in_span].iter().collect();
                new_spans.push(Span::styled(selected, span.style.bg(theme.selection_bg)));
                // After selection
                if sel_end_in_span < span_char_count {
                    let after: String = chars[sel_end_in_span..].iter().collect();
                    new_spans.push(Span::styled(after, span.style));
                }
            }

            char_pos += span_char_count;
        }
        line.spans = new_spans;
    }
}

/// Render diff content into styled lines with box-drawing frame.
fn render_diff_lines(
    lines: &mut Vec<Line<'_>>,
    diff: &DiffContent,
    result_summary: Option<&str>,
    theme: &Theme,
    context_pct: u8,
    content_width: usize,
) {
    match diff {
        DiffContent::EditDiff { lines: diff_lines }
        | DiffContent::PatchDiff { lines: diff_lines } => {
            // Dash count: fill remaining width after the "  ┌"/"  └" prefix (3 chars)
            let dash_count = content_width.saturating_sub(3);
            let border_color = theme.border_color(context_pct);

            // Top border
            lines.push(primitives::diff_border_top(dash_count, border_color));

            for diff_line in diff_lines {
                let (prefix, text, color) = match diff_line {
                    DiffLine::Removal(t) => ("-", t.as_str(), theme.error),
                    DiffLine::Addition(t) => ("+", t.as_str(), theme.success),
                    DiffLine::Context(t) => (" ", t.as_str(), theme.dim),
                    DiffLine::HunkHeader(t) => ("", t.as_str(), theme.dim),
                };
                lines.push(Line::from(vec![
                    Span::styled("  \u{2502} ", Style::default().fg(border_color)),
                    Span::styled(format!("{prefix}{text}"), Style::default().fg(color)),
                ]));
            }

            // Bottom border
            lines.push(primitives::diff_border_bottom(dash_count, border_color));
        }
        DiffContent::WriteSummary { line_count } => {
            // Determine if this is a create or overwrite from the result summary
            let verb = match result_summary {
                Some(s) if s.starts_with("Created") => "Created",
                _ => "Overwrote",
            };
            lines.push(Line::from(Span::styled(
                format!("  {verb} ({line_count} lines)"),
                Style::default().fg(theme.dim),
            )));
        }
    }
}

/// Infer an intent category from the tool calls in a single tool group.
/// Returns `None` if the group has no calls or only `Asking` tools
/// (question/task).
///
/// Priority: editing > executing > exploring. When a group contains
/// mixed tools, the highest-priority wins (mutations matter most).
fn infer_group_intent(group: &ToolGroup) -> Option<IntentCategory> {
    let mut has_exploring = false;
    let mut has_editing = false;
    let mut has_executing = false;

    let mut has_delegating = false;

    for call in &group.calls {
        match call.tool_name.intent_category() {
            IntentCategory::Exploring => has_exploring = true,
            IntentCategory::Editing => has_editing = true,
            IntentCategory::Executing => has_executing = true,
            IntentCategory::Delegating => has_delegating = true,
            IntentCategory::Asking => {} // doesn't influence the label
        }
    }

    if has_editing {
        Some(IntentCategory::Editing)
    } else if has_executing {
        Some(IntentCategory::Executing)
    } else if has_delegating {
        Some(IntentCategory::Delegating)
    } else if has_exploring {
        Some(IntentCategory::Exploring)
    } else {
        None
    }
}

/// Render an intent indicator line: `── label ──────────────────`
///
/// Uses box-drawing `─` chars with the label colored per intent category,
/// reusing existing theme colors for visual consistency with tool call lines.
/// Exhaustive match on `IntentCategory` — adding a variant forces updating this.
fn render_intent_line(category: IntentCategory, width: usize, theme: &Theme) -> Line<'static> {
    let (label, color) = match category {
        IntentCategory::Exploring => ("exploring", theme.tool_read),
        IntentCategory::Editing => ("editing", theme.tool_write),
        IntentCategory::Executing => ("executing", theme.accent),
        IntentCategory::Delegating => ("delegating", theme.accent),
        // Asking is never passed here (infer_group_intent returns None for asking-only groups),
        // but the arm is kept for exhaustive coverage so new variants force an update.
        IntentCategory::Asking => ("asking", theme.dim),
    };

    let prefix = format!("\u{2500}\u{2500} {label} ");
    let prefix_chars = prefix.chars().count();
    let dash_count = width.saturating_sub(prefix_chars);
    let dashes = "\u{2500}".repeat(dash_count);
    let full = format!("{prefix}{dashes}");

    Line::from(Span::styled(full, Style::default().fg(color)))
}

/// Detect fenced code blocks in assistant text and render with tinted background.
///
/// Uses a stateless line-by-line scanner: lines starting with ` ``` ` (≤3 leading
/// spaces) toggle code block mode. Opening fences emit a header line with optional
/// language label; closing fences are consumed. Code lines get `code_bg` background.
fn render_text_with_code_blocks(
    text: &str,
    theme: &Theme,
    available_width: usize,
) -> Vec<MarkdownLine<'static>> {
    let mut result: Vec<MarkdownLine<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut highlighter: Option<syntect::easy::HighlightLines<'_>> = None;
    let mut table_buffer: Vec<String> = Vec::new();

    for text_line in text.lines() {
        match CodeFence::classify(text_line, in_code_block) {
            CodeFence::Open { lang } => {
                // Flush any pending table
                if !table_buffer.is_empty() {
                    result.extend(render_table(&table_buffer, theme, available_width));
                    table_buffer.clear();
                }

                let code_bg_style = Style::default().fg(theme.dim).bg(theme.code_bg);

                // Try to initialize syntax highlighter for this language
                highlighter = syntax::try_highlighter(&lang);

                if !lang.is_empty() {
                    // Language header: "  lang ─────────────────"
                    let label = format!("  {lang} ");
                    let label_chars = label.chars().count();
                    let dash_count = available_width.saturating_sub(label_chars);
                    let dashes = "\u{2500}".repeat(dash_count);
                    let line = Line::from(vec![
                        Span::styled(label.clone(), code_bg_style),
                        Span::styled(dashes, code_bg_style),
                    ])
                    .style(Style::default().bg(theme.code_bg));
                    result.push(MarkdownLine {
                        plain: label.trim().to_string(),
                        styled: line,
                    });
                } else {
                    // No language: emit a thin rule header for visual framing
                    let dashes = "\u{2500}".repeat(available_width);
                    let line = Line::from(Span::styled(dashes, code_bg_style))
                        .style(Style::default().bg(theme.code_bg));
                    result.push(MarkdownLine {
                        plain: String::new(),
                        styled: line,
                    });
                }
                in_code_block = true;
            }
            CodeFence::Close => {
                // Closing rule line
                let code_bg_style = Style::default().fg(theme.dim).bg(theme.code_bg);
                let dashes = "\u{2500}".repeat(available_width);
                let line = Line::from(Span::styled(dashes, code_bg_style))
                    .style(Style::default().bg(theme.code_bg));
                result.push(MarkdownLine {
                    plain: String::new(),
                    styled: line,
                });
                in_code_block = false;
                highlighter = None;
            }
            CodeFence::NotFence if in_code_block => {
                // Code line — try syntax highlighting, fall back to plain
                let code_line = if let Some(ref mut h) = highlighter {
                    if let Ok(regions) = h.highlight_line(text_line, syntax::syntax_set()) {
                        let spans = syntax::syntect_to_spans(&regions, theme.code_bg);
                        Line::from(spans).style(Style::default().bg(theme.code_bg))
                    } else {
                        Line::from(Span::styled(
                            text_line.to_string(),
                            Style::default().fg(theme.assistant_msg).bg(theme.code_bg),
                        ))
                        .style(Style::default().bg(theme.code_bg))
                    }
                } else {
                    Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.assistant_msg).bg(theme.code_bg),
                    ))
                    .style(Style::default().bg(theme.code_bg))
                };
                result.push(MarkdownLine {
                    plain: text_line.to_string(),
                    styled: code_line,
                });
            }
            CodeFence::NotFence => {
                // Table row detection — buffer consecutive table lines
                if is_table_row(text_line) {
                    table_buffer.push(text_line.to_string());
                    continue;
                }

                // Flush any pending table before prose
                if !table_buffer.is_empty() {
                    result.extend(render_table(&table_buffer, theme, available_width));
                    table_buffer.clear();
                }

                // Normal prose line — apply markdown formatting
                result.push(render_markdown_line(text_line, theme, available_width));
            }
        }
    }

    // Flush trailing table
    if !table_buffer.is_empty() {
        result.extend(render_table(&table_buffer, theme, available_width));
    }

    result
}

/// Split a text line into spans, highlighting `@file` and `@!file` references with accent color.
fn style_file_refs<'a>(line: &str, theme: &Theme) -> Vec<Span<'a>> {
    let refs = crate::file_ref::parse_refs(line);
    if refs.is_empty() {
        return vec![Span::styled(
            line.to_string(),
            Style::default().fg(theme.user_msg),
        )];
    }

    let mut spans = Vec::new();
    let mut last_end = 0;

    for r in &refs {
        // Text before this ref
        if r.start > last_end {
            spans.push(Span::styled(
                line[last_end..r.start].to_string(),
                Style::default().fg(theme.user_msg),
            ));
        }
        // The ref itself — highlighted
        spans.push(Span::styled(
            line[r.start..r.end].to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
        last_end = r.end;
    }

    // Trailing text after last ref
    if last_end < line.len() {
        spans.push(Span::styled(
            line[last_end..].to_string(),
            Style::default().fg(theme.user_msg),
        ));
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn default_starts_at_zero_with_auto_scroll() {
        let state = MessageAreaState::default();
        assert_eq!(state.scroll_offset, 0);
        assert!(state.auto_scroll);
    }

    #[test]
    fn update_dimensions_auto_scrolls_to_bottom() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80); // max_scroll = 100 - 20
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_up_disables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        assert_eq!(state.scroll_offset, 80);
        state.scroll_up(5);
        assert_eq!(state.scroll_offset, 75);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(200);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_to_bottom_re_enables_auto_scroll() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(30); // at 50 now
        assert!(!state.auto_scroll);
        state.scroll_down(30); // at 80 = max_scroll
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }

    #[test]
    fn scroll_down_clamps_at_max() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70
        state.scroll_down(200); // should clamp to 80
        assert_eq!(state.scroll_offset, 80);
    }

    #[test]
    fn update_dimensions_clamps_when_not_auto_scrolling() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(10); // at 70, auto_scroll = false
        // Content shrinks (e.g., after compact)
        state.update_dimensions(50, 20);
        // max_scroll = 30, so offset should clamp from 70 to 30
        assert_eq!(state.scroll_offset, 30);
        assert!(!state.auto_scroll);
    }

    #[test]
    fn max_scroll_zero_when_content_fits() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(10, 20);
        assert_eq!(state.max_scroll(), 0);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn scroll_to_bottom_works() {
        let mut state = MessageAreaState::default();
        state.update_dimensions(100, 20);
        state.scroll_up(50); // at 30
        state.scroll_to_bottom();
        assert_eq!(state.scroll_offset, 80);
        assert!(state.auto_scroll);
    }

    // -- Gutter tests --

    use strum::IntoEnumIterator;

    #[test]
    fn tool_color_exhaustive() {
        let theme = Theme::default();
        for t in ToolName::iter() {
            let _color = tool_color(t, &theme);
            // Every variant returns a color without panicking.
        }
    }

    #[test]
    fn gutter_empty_for_text_lines() {
        let theme = Theme::default();
        let line = Line::from("hello");
        let guttered = prepend_gutter(line, GutterMark::Empty, &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("   "),
            "empty gutter should be 3 spaces, got: {text}"
        );
        assert!(text.ends_with("hello"), "content should be preserved");
    }

    #[test]
    fn gutter_marker_for_read_tools() {
        let theme = Theme::default();
        let line = Line::from("read(src/main.rs)");
        let guttered = prepend_gutter(line, GutterMark::ToolMarker(ToolName::Read), &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("\u{00b7}"),
            "read gutter should start with · marker, got: {text}"
        );
        assert!(
            text.contains("\u{2502}"),
            "gutter should contain │ separator"
        );
        assert!(
            text.ends_with("read(src/main.rs)"),
            "content should be preserved"
        );
        // Marker span should have tool_read color
        assert_eq!(guttered.spans[0].style.fg, Some(theme.tool_read));
    }

    #[test]
    fn gutter_marker_for_write_tools() {
        let theme = Theme::default();
        let line = Line::from("edit(src/main.rs)");
        let guttered = prepend_gutter(line, GutterMark::ToolMarker(ToolName::Edit), &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("\u{270e}"),
            "write gutter should start with ✎ marker, got: {text}"
        );
        // Marker span should have tool_write color
        assert_eq!(guttered.spans[0].style.fg, Some(theme.tool_write));
    }

    #[test]
    fn gutter_continuation_for_expanded() {
        let theme = Theme::default();
        let line = Line::from("  +new_code");
        let guttered = prepend_gutter(line, GutterMark::Continuation(ToolName::Edit), &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        // Continuation: "│ │" (two pipes separated by space)
        assert!(
            text.starts_with("\u{2502} \u{2502}"),
            "continuation gutter should be │ │, got: {text}"
        );
        assert!(text.ends_with("+new_code"), "content preserved");
        // Continuation uses dim color
        assert_eq!(guttered.spans[0].style.fg, Some(theme.dim));
    }

    #[test]
    fn gutter_intent_line() {
        let theme = Theme::default();
        let line = Line::from("── exploring ──");
        let guttered = prepend_gutter(line, GutterMark::Intent, &theme);
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("\u{2500} \u{2502}"),
            "intent gutter should be ─ │, got: {text}"
        );
        assert!(text.ends_with("exploring ──"), "content preserved");
    }

    #[test]
    fn prepend_gutter_preserves_content() {
        let theme = Theme::default();
        // Multi-span line with a style
        let line = Line::from(vec![
            Span::styled("hello ", Style::default().fg(theme.user_msg)),
            Span::styled("world", Style::default().fg(theme.accent)),
        ]);
        let guttered = prepend_gutter(line, GutterMark::Empty, &theme);
        // Should have gutter span(s) + original 2 spans
        let text: String = guttered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "   hello world", "gutter + original content");
        // Original spans should be at the end
        let last_two = &guttered.spans[guttered.spans.len() - 2..];
        assert_eq!(last_two[0].content.as_ref(), "hello ");
        assert_eq!(last_two[1].content.as_ref(), "world");
    }

    #[test]
    fn prepend_gutter_preserves_line_style() {
        let theme = Theme::default();
        let line = Line::from("code").style(Style::default().bg(theme.code_bg));
        let guttered = prepend_gutter(line, GutterMark::Empty, &theme);
        assert_eq!(
            guttered.style.bg,
            Some(theme.code_bg),
            "line style should be preserved"
        );
    }

    #[test]
    fn gutter_marker_exhaustive() {
        // Every ToolName variant must return a non-empty 1-column marker.
        for t in ToolName::iter() {
            let m = gutter_marker(t);
            assert!(!m.is_empty(), "{t} gutter marker should be non-empty");
            assert_eq!(
                m.chars().count(),
                1,
                "{t} gutter marker should be exactly 1 char, got '{m}'"
            );
        }
    }

    #[test]
    fn gutter_width_is_three_chars() {
        let theme = Theme::default();
        // All mark types should produce exactly GUTTER_WIDTH (3) chars
        let marks = [
            GutterMark::Empty,
            GutterMark::ToolMarker(ToolName::Read),
            GutterMark::ToolMarker(ToolName::Edit),
            GutterMark::ToolMarker(ToolName::Bash),
            GutterMark::ToolMarker(ToolName::Question),
            GutterMark::Continuation(ToolName::Read),
            GutterMark::Intent,
        ];
        for mark in marks {
            let spans = gutter_spans(mark, &theme);
            let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(
                width, GUTTER_WIDTH,
                "gutter mark {mark:?} should be {GUTTER_WIDTH} chars, got {width}"
            );
        }
    }

    // -- render_diff_lines tests --

    use super::super::{
        message_block::{DiffContent, DiffLine},
        theme::Theme,
    };

    #[test]
    fn render_diff_lines_edit_diff_structure() {
        let theme = Theme::default();
        let diff = DiffContent::EditDiff {
            lines: vec![
                DiffLine::Removal("old".into()),
                DiffLine::Addition("new".into()),
            ],
        };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme, 0, 60);
        // top border + 2 diff lines + bottom border = 4 lines
        assert_eq!(output.len(), 4);
    }

    #[test]
    fn render_diff_lines_patch_diff_structure() {
        let theme = Theme::default();
        let diff = DiffContent::PatchDiff {
            lines: vec![
                DiffLine::HunkHeader("@@ -1 +1 @@".into()),
                DiffLine::Context("ctx".into()),
                DiffLine::Removal("old".into()),
                DiffLine::Addition("new".into()),
            ],
        };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme, 0, 60);
        // top border + 4 diff lines + bottom border = 6 lines
        assert_eq!(output.len(), 6);
    }

    #[test]
    fn render_diff_lines_write_summary_created_verb() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 10 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(
            &mut output,
            &diff,
            Some("Created /tmp/foo (42 bytes)"),
            &theme,
            0,
            60,
        );
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(text.contains("Created"), "verb should be Created");
        assert!(text.contains("10 lines"), "should show line count");
    }

    #[test]
    fn render_diff_lines_write_summary_overwrote_verb() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 5 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(
            &mut output,
            &diff,
            Some("Overwrote /tmp/foo (20 bytes)"),
            &theme,
            0,
            60,
        );
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(text.contains("Overwrote"), "verb should be Overwrote");
    }

    #[test]
    fn render_diff_lines_write_summary_defaults_to_overwrote() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 3 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme, 0, 60);
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(
            text.contains("Overwrote"),
            "should default to Overwrote when no summary"
        );
    }

    #[test]
    fn render_diff_lines_empty_edit_diff() {
        let theme = Theme::default();
        let diff = DiffContent::EditDiff { lines: vec![] };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme, 0, 60);
        // top border + 0 diff lines + bottom border = 2 lines
        assert_eq!(output.len(), 2);
    }

    // -- Buffer rendering tests --
    //
    // These test the actual render pipeline through ratatui's TestBackend,
    // catching layout bugs that pure data-model tests miss.

    use super::super::message_block::{ThinkingBlock, ToolCall, ToolGroup, ToolGroupStatus};
    use ratatui::layout::Rect;

    /// Helper: render message blocks into a buffer and return the buffer text as a single string.
    fn render_messages_to_string(
        width: u16,
        height: u16,
        messages: &[MessageBlock],
        activity: Option<(char, String, bool, Option<Duration>)>,
    ) -> String {
        let theme = Theme::default();
        let mut state = MessageAreaState::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_message_blocks(
                frame,
                Rect::new(0, 0, width, height),
                messages,
                &mut state,
                &theme,
                activity,
                0, // context_pct: no pressure in tests
                &SelectionState::default(),
            );
        });
        // Collect all cells into a string, row by row
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
    fn buffer_user_message_has_prefix() {
        let messages = vec![MessageBlock::User {
            text: "Hello world".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(
            text.contains("│ Hello world"),
            "user message should have '│ ' prefix, got:\n{text}"
        );
    }

    #[test]
    fn buffer_assistant_text_rendered() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text("Response text here".to_string())],
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(
            text.contains("Response text here"),
            "assistant text should appear, got:\n{text}"
        );
        // Should NOT have "│ " prefix
        assert!(
            !text.contains("│ Response"),
            "assistant should not have user prefix"
        );
    }

    #[test]
    fn buffer_thinking_collapsed_shows_arrow_and_token_count() {
        let messages = vec![MessageBlock::Assistant {
            thinking: Some(ThinkingBlock {
                token_count: 42,
                content: "deep thoughts".to_string(),
                expanded: false,
            }),
            parts: vec![],
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(
            text.contains("\u{25b6}"),
            "collapsed thinking should show ▶"
        );
        assert!(text.contains("Thinking"), "should show 'Thinking'");
        assert!(text.contains("42"), "should show token count");
    }

    #[test]
    fn buffer_thinking_expanded_shows_content() {
        let messages = vec![MessageBlock::Assistant {
            thinking: Some(ThinkingBlock {
                token_count: 10,
                content: "my thoughts".to_string(),
                expanded: true,
            }),
            parts: vec![],
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("\u{25bc}"), "expanded thinking should show ▼");
        assert!(
            text.contains("my thoughts"),
            "expanded thinking should show content"
        );
    }

    #[test]
    fn buffer_tool_group_preparing_shows_spinner() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".to_string(),
                    full_output: None,
                    result_summary: None,
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Preparing,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(
            text.contains("preparing..."),
            "preparing tool should show 'preparing...'"
        );
    }

    #[test]
    fn buffer_tool_group_complete_collapsed() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".to_string(),
                    full_output: Some("file content".to_string()),
                    result_summary: Some("150 lines".to_string()),
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(
            text.contains("\u{25b6}"),
            "collapsed complete should show ▶"
        );
        assert!(text.contains("read"), "should show tool name");
        assert!(text.contains("src/main.rs"), "should show args");
        assert!(text.contains("150 lines"), "should show result summary");
    }

    #[test]
    fn buffer_tool_group_expanded_with_diff() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Edit,
                    args_summary: "src/main.rs".to_string(),
                    full_output: None,
                    result_summary: Some("edited".to_string()),
                    diff_content: Some(DiffContent::EditDiff {
                        lines: vec![
                            DiffLine::Removal("old line".into()),
                            DiffLine::Addition("new line".into()),
                        ],
                    }),
                    is_error: false,
                    expanded: true,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        // Box-drawing frame
        assert!(text.contains("\u{250c}"), "should have top-left corner ┌");
        assert!(
            text.contains("\u{2514}"),
            "should have bottom-left corner └"
        );
        assert!(text.contains("-old line"), "should show removal with -");
        assert!(text.contains("+new line"), "should show addition with +");
    }

    #[test]
    fn buffer_system_message_rendered() {
        let messages = vec![MessageBlock::System {
            text: "System notice".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(
            text.contains("System notice"),
            "system message should appear"
        );
    }

    #[test]
    fn buffer_error_message_rendered() {
        let messages = vec![MessageBlock::Error {
            text: "Something broke".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(
            text.contains("Something broke"),
            "error message should appear"
        );
    }

    #[test]
    fn buffer_permission_prompt_rendered() {
        let messages = vec![MessageBlock::Permission {
            tool_name: "bash".to_string(),
            args_summary: "rm -rf".to_string(),
            diff_content: None,
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("Allow"), "permission should show 'Allow'");
        assert!(text.contains("bash"), "permission should show tool name");
        assert!(text.contains("rm -rf"), "permission should show args");
        assert!(text.contains("]es"), "should show [y]es option");
        assert!(text.contains("]o"), "should show [n]o option");
        assert!(text.contains("]lways"), "should show [a]lways option");
    }

    #[test]
    fn buffer_permission_prompt_with_diff_preview() {
        use crate::ui::message_block::{DiffContent, DiffLine};
        let messages = vec![MessageBlock::Permission {
            tool_name: "edit".to_string(),
            args_summary: "Edit file: src/main.rs".to_string(),
            diff_content: Some(DiffContent::EditDiff {
                lines: vec![
                    DiffLine::Removal("old code".into()),
                    DiffLine::Addition("new code".into()),
                ],
            }),
        }];
        let text = render_messages_to_string(60, 20, &messages, None);
        assert!(text.contains("Allow"), "should show Allow prompt");
        assert!(text.contains("edit"), "should show tool name");
        // Diff content should appear between the title and options
        assert!(text.contains("old code"), "should show removed line");
        assert!(text.contains("new code"), "should show added line");
        assert!(text.contains("]es"), "should show [y]es option");
    }

    #[test]
    fn buffer_activity_spinner_inline() {
        let messages = vec![];
        let text = render_messages_to_string(
            60,
            10,
            &messages,
            Some(('⠋', "Thinking...".to_string(), false, None)),
        );
        assert!(text.contains("Thinking..."), "activity text should appear");
    }

    #[test]
    fn buffer_blank_line_between_messages() {
        let messages = vec![
            MessageBlock::User {
                text: "msg1".to_string(),
            },
            MessageBlock::User {
                text: "msg2".to_string(),
            },
        ];
        let text = render_messages_to_string(60, 10, &messages, None);
        // Find positions — msg2 should not immediately follow msg1
        let pos1 = text.find("│ msg1").expect("msg1 not found");
        let pos2 = text.find("│ msg2").expect("msg2 not found");
        // There should be at least one blank line between them (newline + spaces + newline)
        let between = &text[pos1..pos2];
        let line_count = between.lines().count();
        assert!(
            line_count >= 2,
            "should have blank line separation, got {line_count} lines between messages"
        );
    }

    // -- infer_group_intent tests --

    fn make_tool_group(tool_names: &[ToolName]) -> ToolGroup {
        ToolGroup {
            calls: tool_names
                .iter()
                .map(|&name| ToolCall {
                    tool_name: name,
                    args_summary: "test".into(),
                    full_output: None,
                    result_summary: None,
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                })
                .collect(),
            status: ToolGroupStatus::Complete,
        }
    }

    #[test]
    fn infer_group_intent_empty() {
        let group = make_tool_group(&[]);
        assert_eq!(infer_group_intent(&group), None);
    }

    #[test]
    fn infer_group_intent_read_only_tools() {
        for tool in [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::List,
            ToolName::Webfetch,
        ] {
            let group = make_tool_group(&[tool]);
            assert_eq!(
                infer_group_intent(&group),
                Some(IntentCategory::Exploring),
                "{tool} should produce Exploring"
            );
        }
    }

    #[test]
    fn infer_group_intent_write_tools() {
        for tool in [
            ToolName::Edit,
            ToolName::Write,
            ToolName::Patch,
            ToolName::Move,
            ToolName::Copy,
            ToolName::Delete,
            ToolName::Mkdir,
            ToolName::Memory,
        ] {
            let group = make_tool_group(&[tool]);
            assert_eq!(
                infer_group_intent(&group),
                Some(IntentCategory::Editing),
                "{tool} should produce Editing"
            );
        }
    }

    #[test]
    fn infer_group_intent_bash_tool() {
        let group = make_tool_group(&[ToolName::Bash]);
        assert_eq!(infer_group_intent(&group), Some(IntentCategory::Executing));
    }

    #[test]
    fn infer_group_intent_mixed_read_write() {
        let group = make_tool_group(&[ToolName::Read, ToolName::Edit]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Editing),
            "editing takes priority over exploring"
        );
    }

    #[test]
    fn infer_group_intent_mixed_read_bash() {
        let group = make_tool_group(&[ToolName::Read, ToolName::Bash]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Executing),
            "executing takes priority over exploring"
        );
    }

    #[test]
    fn infer_group_intent_mixed_edit_bash() {
        let group = make_tool_group(&[ToolName::Edit, ToolName::Bash]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Editing),
            "editing takes priority over executing"
        );
    }

    #[test]
    fn infer_group_intent_only_asking_tools() {
        let group = make_tool_group(&[ToolName::Question, ToolName::Task]);
        assert_eq!(
            infer_group_intent(&group),
            None,
            "asking tools should not produce a label"
        );
    }

    #[test]
    fn infer_group_intent_asking_plus_exploring() {
        let group = make_tool_group(&[ToolName::Task, ToolName::Read]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Exploring),
            "exploring should show even with asking tools"
        );
    }

    #[test]
    fn infer_group_intent_all_categories() {
        let group = make_tool_group(&[
            ToolName::Read,
            ToolName::Edit,
            ToolName::Bash,
            ToolName::Question,
        ]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Editing),
            "editing has highest priority"
        );
    }

    #[test]
    fn infer_group_intent_agent_tool() {
        let group = make_tool_group(&[ToolName::Agent]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Delegating),
            "agent should produce Delegating"
        );
    }

    #[test]
    fn infer_group_intent_agent_plus_read() {
        let group = make_tool_group(&[ToolName::Read, ToolName::Agent]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Delegating),
            "delegating takes priority over exploring"
        );
    }

    #[test]
    fn infer_group_intent_agent_priority() {
        // editing > executing > delegating > exploring
        let group = make_tool_group(&[ToolName::Agent, ToolName::Read]);
        assert_eq!(infer_group_intent(&group), Some(IntentCategory::Delegating));

        let group = make_tool_group(&[ToolName::Agent, ToolName::Bash]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Executing),
            "executing takes priority over delegating"
        );

        let group = make_tool_group(&[ToolName::Agent, ToolName::Edit]);
        assert_eq!(
            infer_group_intent(&group),
            Some(IntentCategory::Editing),
            "editing takes priority over delegating"
        );
    }

    // -- render_intent_line tests --

    #[test]
    fn render_intent_line_format() {
        let theme = Theme::default();
        let line = render_intent_line(IntentCategory::Exploring, 40, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with("\u{2500}\u{2500} exploring "),
            "should start with '── exploring '"
        );
        assert!(text.ends_with('\u{2500}'), "should end with ─ dashes");
        assert_eq!(text.chars().count(), 40, "should fill to width");
        // Color should be tool_read for exploring
        assert_eq!(line.spans[0].style.fg, Some(theme.tool_read));
    }

    #[test]
    fn render_intent_line_editing_color() {
        let theme = Theme::default();
        let line = render_intent_line(IntentCategory::Editing, 30, &theme);
        assert_eq!(line.spans[0].style.fg, Some(theme.tool_write));
    }

    #[test]
    fn render_intent_line_executing_color() {
        let theme = Theme::default();
        let line = render_intent_line(IntentCategory::Executing, 30, &theme);
        assert_eq!(line.spans[0].style.fg, Some(theme.accent));
    }

    #[test]
    fn render_intent_line_delegating() {
        let theme = Theme::default();
        let line = render_intent_line(IntentCategory::Delegating, 40, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("delegating"));
        assert_eq!(line.spans[0].style.fg, Some(theme.accent));
    }

    #[test]
    fn render_intent_line_narrow_width() {
        let theme = Theme::default();
        // Width exactly equal to prefix — no trailing dashes
        let prefix_len = "\u{2500}\u{2500} exploring ".chars().count(); // 13 chars
        let line = render_intent_line(IntentCategory::Exploring, prefix_len, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            text, "\u{2500}\u{2500} exploring ",
            "at exact prefix width, no dashes appended"
        );
        assert_eq!(
            text.chars().count(),
            prefix_len,
            "should be exactly prefix length"
        );

        // Width smaller than prefix — no dashes, no panic
        let line2 = render_intent_line(IntentCategory::Exploring, 5, &theme);
        let text2: String = line2.spans.iter().map(|s| s.content.as_ref()).collect();
        // Still contains the full prefix (saturating_sub produces 0 dashes)
        assert!(
            text2.starts_with("\u{2500}\u{2500} exploring "),
            "prefix always rendered"
        );
        assert_eq!(
            text2.chars().count(),
            prefix_len,
            "output is prefix-length when width < prefix"
        );
    }

    // -- per-group intent rendering buffer tests --

    #[test]
    fn buffer_intent_per_group_shows_exploring_then_editing() {
        // Two tool groups: read (exploring) then edit (editing).
        // Each should get its own intent label.
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "src/main.rs".into(),
                        full_output: None,
                        result_summary: Some("150 lines".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
                AssistantPart::Text("Let me edit that.".into()),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Edit,
                        args_summary: "src/main.rs".into(),
                        full_output: None,
                        result_summary: Some("edited".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        assert!(
            text.contains("exploring"),
            "should have exploring label for read group"
        );
        assert!(
            text.contains("editing"),
            "should have editing label for edit group"
        );
        // exploring should appear before editing in the output
        let pos_exploring = text.find("exploring").unwrap();
        let pos_editing = text.find("editing").unwrap();
        assert!(
            pos_exploring < pos_editing,
            "exploring should come before editing"
        );
    }

    #[test]
    fn buffer_intent_no_label_for_text_only() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text("Just a text response.".into())],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(
            !text.contains("exploring"),
            "no intent label for text-only response"
        );
        assert!(
            !text.contains("editing"),
            "no intent label for text-only response"
        );
        assert!(
            !text.contains("executing"),
            "no intent label for text-only response"
        );
    }

    #[test]
    fn buffer_intent_no_label_for_asking_only() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Question,
                    args_summary: "test".into(),
                    full_output: None,
                    result_summary: Some("answered".into()),
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(
            !text.contains("exploring"),
            "no exploring label for question tool"
        );
        assert!(
            !text.contains("editing"),
            "no editing label for question tool"
        );
        assert!(
            !text.contains("executing"),
            "no executing label for question tool"
        );
    }

    #[test]
    fn buffer_intent_consecutive_same_category_deduped() {
        // Three consecutive exploring groups (like list → glob → read across turns).
        // Should produce ONE "exploring" label, not three.
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::List,
                        args_summary: ".".into(),
                        full_output: None,
                        result_summary: Some("10 files".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Glob,
                        args_summary: "src/**/*.rs".into(),
                        full_output: None,
                        result_summary: Some("5 files".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "src/main.rs".into(),
                        full_output: None,
                        result_summary: Some("150 lines".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        // "exploring" should appear exactly once
        let count = text.matches("exploring").count();
        assert_eq!(count, 1, "expected 1 'exploring' label but found {count}");
    }

    #[test]
    fn buffer_intent_text_resets_dedup() {
        // exploring → text → exploring should show TWO "exploring" labels
        // because text between groups resets the tracking.
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "a.rs".into(),
                        full_output: None,
                        result_summary: Some("50 lines".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
                AssistantPart::Text("I see, let me check another file.".into()),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "b.rs".into(),
                        full_output: None,
                        result_summary: Some("30 lines".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        let count = text.matches("exploring").count();
        assert_eq!(
            count, 2,
            "expected 2 'exploring' labels (text resets dedup) but found {count}"
        );
    }

    #[test]
    fn buffer_intent_category_change_shows_both() {
        // exploring → editing (no text between) → both labels shown.
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "a.rs".into(),
                        full_output: None,
                        result_summary: Some("50 lines".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Edit,
                        args_summary: "a.rs".into(),
                        full_output: None,
                        result_summary: Some("edited".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        assert!(text.contains("exploring"), "should have exploring label");
        assert!(text.contains("editing"), "should have editing label");
        let pos_exploring = text.find("exploring").unwrap();
        let pos_editing = text.find("editing").unwrap();
        assert!(pos_exploring < pos_editing, "exploring before editing");
    }

    #[test]
    fn buffer_intent_asking_group_resets_dedup() {
        // exploring → asking-only → exploring should show TWO "exploring" labels
        // because asking-only groups reset intent tracking (they emit no label).
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "a.rs".into(),
                        full_output: None,
                        result_summary: Some("50 lines".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Question,
                        args_summary: "proceed?".into(),
                        full_output: None,
                        result_summary: Some("yes".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
                AssistantPart::ToolGroup(ToolGroup {
                    calls: vec![ToolCall {
                        tool_name: ToolName::Read,
                        args_summary: "b.rs".into(),
                        full_output: None,
                        result_summary: Some("30 lines".into()),
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        let count = text.matches("exploring").count();
        assert_eq!(
            count, 2,
            "expected 2 'exploring' labels (asking resets dedup) but found {count}"
        );
    }

    // -- render_text_with_code_blocks tests --

    #[test]
    fn code_block_renders_with_header() {
        let theme = Theme::default();
        let text = "before\n```rust\nfn main() {}\n```\nafter";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        // "before", header, "fn main() {}", closing rule, "after" = 5 output lines
        assert_eq!(ml.len(), 5, "expected 5 lines, got {}", ml.len());
        // Header should contain language label with dash fill
        let header_text: String = ml[1]
            .styled
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            header_text.contains("rust"),
            "header should contain 'rust', got: {header_text}"
        );
        assert!(
            header_text.contains("\u{2500}"),
            "header should contain dash fill"
        );
    }

    #[test]
    fn code_block_no_language_skips_header() {
        let theme = Theme::default();
        let text = "```\ncode\n```";
        let ml = render_text_with_code_blocks(text, &theme, 30);
        // Header rule + code line + closing rule = 3 lines
        assert_eq!(ml.len(), 3, "expected 3 lines, got {}", ml.len());
        // Middle line is the code
        let code_text: String = ml[1]
            .styled
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(code_text, "code");
        assert_eq!(
            ml[1].styled.style.bg,
            Some(theme.code_bg),
            "code line should have code_bg"
        );
        // Header and closing should have code_bg
        assert_eq!(
            ml[0].styled.style.bg,
            Some(theme.code_bg),
            "header rule should have code_bg"
        );
        assert_eq!(
            ml[2].styled.style.bg,
            Some(theme.code_bg),
            "closing rule should have code_bg"
        );
    }

    #[test]
    fn unclosed_code_block_tints_remaining() {
        let theme = Theme::default();
        let text = "before\n```python\nline1\nline2";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        // "before", header, "line1", "line2" = 4 lines
        assert_eq!(ml.len(), 4);
        // Lines 2 and 3 (code lines) should have code_bg background on Line.style
        for i in 2..4 {
            assert_eq!(
                ml[i].styled.style.bg,
                Some(theme.code_bg),
                "unclosed code line {i} should have code_bg"
            );
        }
    }

    #[test]
    fn empty_code_block() {
        let theme = Theme::default();
        let text = "```\n```";
        let ml = render_text_with_code_blocks(text, &theme, 20);
        // Header rule + closing rule = 2 lines (no code content in between)
        assert_eq!(
            ml.len(),
            2,
            "expected 2 lines (header + closing), got {}",
            ml.len()
        );
        assert_eq!(
            ml[0].styled.style.bg,
            Some(theme.code_bg),
            "header rule should have code_bg"
        );
        assert_eq!(
            ml[1].styled.style.bg,
            Some(theme.code_bg),
            "closing rule should have code_bg"
        );
    }

    #[test]
    fn inline_backticks_not_treated_as_fence() {
        let theme = Theme::default();
        let text = "use `foo` and ``bar``";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        assert_eq!(ml.len(), 1);
        // Should have no code_bg on the Line.style (inline code bg is on individual spans)
        assert_eq!(
            ml[0].styled.style.bg, None,
            "inline backticks should not trigger code block"
        );
    }

    #[test]
    fn multiple_code_blocks() {
        let theme = Theme::default();
        let text = "text1\n```rust\nfn a() {}\n```\ntext2\n```go\nfunc b() {}\n```\ntext3";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        // text1, header1, "fn a() {}", close1, text2, header2, "func b() {}", close2, text3 = 9 lines
        assert_eq!(ml.len(), 9, "expected 9 lines, got {}", ml.len());
        // Normal text lines should NOT have code_bg
        assert_eq!(ml[0].styled.style.bg, None, "text1 should not have bg");
        assert_eq!(ml[4].styled.style.bg, None, "text2 should not have bg");
        assert_eq!(ml[8].styled.style.bg, None, "text3 should not have bg");
        // Code lines should have code_bg
        assert_eq!(
            ml[2].styled.style.bg,
            Some(theme.code_bg),
            "code line 1 should have bg"
        );
        assert_eq!(
            ml[6].styled.style.bg,
            Some(theme.code_bg),
            "code line 2 should have bg"
        );
    }

    #[test]
    fn deeply_indented_fence_ignored() {
        let theme = Theme::default();
        let text = "    ```rust\nstill normal";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        // 4 spaces = not a fence, both lines rendered as normal text
        assert_eq!(ml.len(), 2);
        assert_eq!(
            ml[0].styled.style.bg, None,
            "4-space indented fence should be normal text"
        );
        assert_eq!(
            ml[1].styled.style.bg, None,
            "following line should be normal text"
        );
    }

    #[test]
    fn code_block_header_has_bg() {
        let theme = Theme::default();
        let text = "```js\nconsole.log();\n```";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        // Header line should have code_bg on Line.style
        assert_eq!(
            ml[0].styled.style.bg,
            Some(theme.code_bg),
            "header line should have code_bg background"
        );
    }

    #[test]
    fn fence_closes_code_block() {
        let theme = Theme::default();
        let text = "```\ncode\n```\nafter";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        // Header rule + "code" + closing rule + "after" = 4 lines
        assert_eq!(ml.len(), 4, "expected 4 lines, got {}", ml.len());
        assert_eq!(
            ml[0].styled.style.bg,
            Some(theme.code_bg),
            "header rule should have code_bg"
        );
        assert_eq!(
            ml[1].styled.style.bg,
            Some(theme.code_bg),
            "code line should have code_bg"
        );
        assert_eq!(
            ml[2].styled.style.bg,
            Some(theme.code_bg),
            "closing rule should have code_bg"
        );
        assert_eq!(
            ml[3].styled.style.bg, None,
            "line after closing fence should be normal text"
        );
    }

    #[test]
    fn tab_indented_fence_ignored() {
        let theme = Theme::default();
        let text = "\t```rust\nstill normal";
        let ml = render_text_with_code_blocks(text, &theme, 40);
        // Tab is not a space — fence should not be recognized
        assert_eq!(ml.len(), 2);
        assert_eq!(
            ml[0].styled.style.bg, None,
            "tab-indented fence should be normal text"
        );
        assert_eq!(
            ml[1].styled.style.bg, None,
            "following line should be normal text"
        );
    }

    #[test]
    fn buffer_code_block_renders_with_tint() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text(
                "Here is code:\n```rust\nfn main() {}\n```\nDone.".to_string(),
            )],
        }];
        let text = render_messages_to_string(60, 15, &messages, None);
        // The header should appear with language label
        assert!(
            text.contains("rust"),
            "should contain language label 'rust'"
        );
        // Code block framing now uses ─ for header/footer rules
        assert!(
            text.contains('\u{2500}'),
            "should contain ─ in code block framing"
        );
        // The code line should appear
        assert!(text.contains("fn main() {}"), "should contain code content");
        // The fence lines (```) should NOT appear
        assert!(
            !text.contains("```"),
            "fence markers should be consumed, not rendered"
        );
        // Normal text should appear
        assert!(
            text.contains("Here is code:"),
            "text before block should appear"
        );
        assert!(text.contains("Done."), "text after block should appear");
    }

    // -- Syntax highlighting tests --

    #[test]
    fn code_block_with_known_lang_has_multiple_spans() {
        let theme = Theme::default();
        let text = "```rust\nfn main() { let x = 42; }\n```";
        let ml = render_text_with_code_blocks(text, &theme, 80);
        // lines: header, code line, closing rule = 3 output lines
        assert_eq!(ml.len(), 3, "expected 3 lines, got {}", ml.len());
        // The code line (index 1) should have multiple spans from syntax highlighting
        assert!(
            ml[1].styled.spans.len() > 1,
            "highlighted code should produce >1 span, got {} spans: {:?}",
            ml[1].styled.spans.len(),
            ml[1]
                .styled
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
        // All spans should have code_bg background
        for span in &ml[1].styled.spans {
            assert_eq!(
                span.style.bg,
                Some(theme.code_bg),
                "highlighted span should have code_bg background"
            );
        }
    }

    #[test]
    fn code_block_unknown_lang_single_span() {
        let theme = Theme::default();
        let text = "```nonexistent_gibberish_42\nsome code here\n```";
        let ml = render_text_with_code_blocks(text, &theme, 80);
        // lines: header, code line, closing rule = 3 output lines
        assert_eq!(ml.len(), 3, "expected 3 lines, got {}", ml.len());
        // Unknown lang falls back to plain rendering — 1 span with assistant_msg fg
        assert_eq!(
            ml[1].styled.spans.len(),
            1,
            "unknown lang should produce 1 span (plain fallback)"
        );
        assert_eq!(
            ml[1].styled.spans[0].style.fg,
            Some(theme.assistant_msg),
            "fallback span should use assistant_msg foreground"
        );
    }

    // -- Integration test: full render pipeline --

    // -- Question block rendering tests --

    #[test]
    fn buffer_question_with_options_active() {
        let messages = vec![MessageBlock::Question {
            question: "What approach?".to_string(),
            options: vec!["Option A".to_string(), "Option B".to_string()],
            selected: Some(0),
            free_text: String::new(),
            answered: None,
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        assert!(
            text.contains("? What approach?"),
            "should show question, got:\n{text}"
        );
        assert!(
            text.contains("1. Option A"),
            "should show option 1, got:\n{text}"
        );
        assert!(
            text.contains("2. Option B"),
            "should show option 2, got:\n{text}"
        );
        assert!(
            text.contains("Enter"),
            "should show Enter key hint, got:\n{text}"
        );
        assert!(
            text.contains("Esc"),
            "should show Esc key hint, got:\n{text}"
        );
    }

    #[test]
    fn buffer_question_answered() {
        let messages = vec![MessageBlock::Question {
            question: "What approach?".to_string(),
            options: vec!["Option A".to_string()],
            selected: Some(0),
            free_text: String::new(),
            answered: Some("Option A".to_string()),
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        assert!(
            text.contains("? What approach?"),
            "should show question, got:\n{text}"
        );
        assert!(
            text.contains("Option A"),
            "should show answer, got:\n{text}"
        );
        // Should NOT show the help line when answered
        assert!(
            !text.contains("Enter"),
            "should not show key hints when answered, got:\n{text}"
        );
    }

    #[test]
    fn buffer_question_free_text_mode() {
        let messages = vec![MessageBlock::Question {
            question: "What do you think?".to_string(),
            options: vec![],
            selected: None,
            free_text: "my answer".to_string(),
            answered: None,
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        assert!(
            text.contains("? What do you think?"),
            "should show question, got:\n{text}"
        );
        assert!(
            text.contains("my answer"),
            "should show free text input, got:\n{text}"
        );
    }

    #[test]
    fn table_renders_with_box_drawing() {
        let theme = Theme::default();
        let text = "| Name | Age |\n|------|-----|\n| Alice | 30 |";
        let ml = render_text_with_code_blocks(text, &theme, 60);
        assert_eq!(ml.len(), 3, "header + separator + data row");
        // Header should contain Name
        assert!(ml[0].plain.contains("Name"), "header should contain Name");
        // Separator should use box-drawing chars
        assert!(ml[1].plain.contains("─"), "separator should use ─");
        // Data should contain Alice
        assert!(ml[2].plain.contains("Alice"), "data should contain Alice");
    }

    #[test]
    fn table_followed_by_prose() {
        let theme = Theme::default();
        let text = "| A | B |\n|---|---|\n| 1 | 2 |\n\nSome prose after";
        let ml = render_text_with_code_blocks(text, &theme, 60);
        // Table (3 lines) + empty line + prose = 5 lines
        assert_eq!(ml.len(), 5, "expected 5 lines, got {}", ml.len());
        assert!(
            ml[4].plain.contains("Some prose after"),
            "prose after table"
        );
    }

    // -- Phase review: missing test coverage --

    #[test]
    fn buffer_empty_state_welcome() {
        let text = render_messages_to_string(60, 20, &[], None);
        assert!(
            text.contains("steve"),
            "welcome should show 'steve', got:\n{text}"
        );
        assert!(
            text.contains("Type a message"),
            "welcome should show subtitle, got:\n{text}"
        );
    }

    #[test]
    fn buffer_activity_spinner_message_queued() {
        let text = render_messages_to_string(
            80,
            10,
            &[],
            Some(('\u{280b}', "Running edit...".to_string(), true, None)),
        );
        assert!(
            text.contains("Running edit..."),
            "activity text should appear"
        );
        assert!(
            text.contains("message queued"),
            "should show '(message queued)' when has_pending_input, got:\n{text}"
        );
    }

    #[test]
    fn buffer_system_message_has_rule() {
        let messages = vec![MessageBlock::System {
            text: "Session started".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(
            text.contains("Session started"),
            "system text should appear"
        );
        assert!(
            text.contains("\u{2500}\u{2500}"),
            "system message should have ── rule prefix, got:\n{text}"
        );
    }

    #[test]
    fn buffer_question_shows_selecting_badge() {
        let messages = vec![MessageBlock::Question {
            question: "Pick one".to_string(),
            options: vec!["A".to_string(), "B".to_string()],
            selected: Some(0),
            free_text: String::new(),
            answered: None,
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        assert!(
            text.contains("[selecting]"),
            "unanswered with selection should show [selecting], got:\n{text}"
        );
    }

    #[test]
    fn buffer_question_shows_typing_badge() {
        let messages = vec![MessageBlock::Question {
            question: "What?".to_string(),
            options: vec![],
            selected: None,
            free_text: "typing here".to_string(),
            answered: None,
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        assert!(
            text.contains("[typing]"),
            "free-text mode should show [typing], got:\n{text}"
        );
    }

    #[test]
    fn buffer_question_answered_no_badge() {
        let messages = vec![MessageBlock::Question {
            question: "Pick one".to_string(),
            options: vec!["A".to_string()],
            selected: Some(0),
            free_text: String::new(),
            answered: Some("A".to_string()),
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        assert!(
            !text.contains("[selecting]"),
            "answered should not show badge"
        );
        assert!(!text.contains("[typing]"), "answered should not show badge");
    }

    #[test]
    fn buffer_tool_call_drop_parens_format() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".to_string(),
                    full_output: Some("content".to_string()),
                    result_summary: Some("50 lines".to_string()),
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        // New format: tool name followed by space and args (no parens)
        assert!(
            !text.contains("read("),
            "should NOT have parens format 'read(', got:\n{text}"
        );
        assert!(text.contains("read"), "should show tool name");
        assert!(text.contains("src/main.rs"), "should show args");
    }

    #[test]
    fn buffer_scroll_indicator_not_shown_at_bottom() {
        // When auto_scroll is true (default), no indicator should appear
        let text = render_messages_to_string(60, 10, &[], None);
        assert!(
            !text.contains("lines above"),
            "should not show scroll indicator when at bottom"
        );
    }

    #[test]
    fn buffer_user_message_has_bg_tint() {
        let messages = vec![MessageBlock::User {
            text: "Hello".to_string(),
        }];
        let theme = Theme::default();
        let mut state = MessageAreaState::default();
        let buf = super::super::render_to_buffer(60, 10, |frame| {
            render_message_blocks(
                frame,
                Rect::new(0, 0, 60, 10),
                &messages,
                &mut state,
                &theme,
                None,
                0,
                &SelectionState::default(),
            );
        });
        // Find a cell in the user message row that has the user_msg_bg color
        let mut found_bg = false;
        for y in 0..10 {
            for x in 0..60 {
                let cell = &buf[(x, y)];
                if cell.bg == theme.user_msg_bg {
                    found_bg = true;
                    break;
                }
            }
            if found_bg {
                break;
            }
        }
        assert!(
            found_bg,
            "user message should have theme.user_msg_bg background"
        );
    }

    #[test]
    fn buffer_agent_progress_shows_tool_info() {
        use crate::ui::message_block::AgentProgressInfo;

        // Agent tool call with live progress (no result yet)
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Agent,
                    args_summary: "(explore): analyze codebase".to_string(),
                    full_output: None,
                    result_summary: None,
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: Some(AgentProgressInfo {
                        tool_name: ToolName::Read,
                        args_summary: "src/main.rs".into(),
                        result_summary: None,
                        tool_count: 3,
                    }),
                }],
                status: ToolGroupStatus::Running {
                    current_tool: ToolName::Agent,
                },
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        // The agent tool call header should be present
        assert!(
            text.contains("agent"),
            "should show agent tool name, got:\n{text}"
        );
        // The progress line should show the sub-agent's current tool and args
        assert!(
            text.contains("read"),
            "should show sub-agent tool name 'read', got:\n{text}"
        );
        assert!(
            text.contains("src/main.rs"),
            "should show sub-agent args, got:\n{text}"
        );
        // Progress sub-line format: "    read src/main.rs ..."
        // (must match the specific format, not just "..." which appears in "running...")
        assert!(
            text.contains("read src/main.rs ..."),
            "progress sub-line should show 'tool args ...' format, got:\n{text}"
        );
    }

    #[test]
    fn buffer_agent_progress_shows_result() {
        use crate::ui::message_block::AgentProgressInfo;

        // Agent tool call with completed sub-tool (has result summary)
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Agent,
                    args_summary: "(explore): find usages".to_string(),
                    full_output: None,
                    result_summary: None,
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: Some(AgentProgressInfo {
                        tool_name: ToolName::Grep,
                        args_summary: "ToolName".into(),
                        result_summary: Some("12 matches".into()),
                        tool_count: 5,
                    }),
                }],
                status: ToolGroupStatus::Running {
                    current_tool: ToolName::Agent,
                },
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        // Should show the arrow and result instead of "..."
        assert!(
            text.contains("grep"),
            "should show sub-agent tool name 'grep', got:\n{text}"
        );
        assert!(
            text.contains("ToolName"),
            "should show sub-agent args, got:\n{text}"
        );
        assert!(
            text.contains("\u{2192}"),
            "should show arrow for completed result, got:\n{text}"
        );
        assert!(
            text.contains("12 matches"),
            "should show result summary, got:\n{text}"
        );
        // The progress line itself should use arrow, not "..."
        // (Note: the header "running..." is separate from the progress line)
        assert!(
            text.contains("\u{2192} 12 matches"),
            "progress line should show arrow + result, got:\n{text}"
        );
    }

    // --- apply_selection_highlight tests ---

    use ratatui::style::Color;

    /// Helper: build a line with a gutter (3 chars) + content spans
    fn make_line_with_gutter<'a>(content_spans: Vec<Span<'a>>) -> Line<'a> {
        let mut spans = vec![Span::raw(" · ")]; // 3-char gutter
        spans.extend(content_spans);
        Line::from(spans)
    }

    #[test]
    fn selection_highlight_partial_span_splits_correctly() {
        let theme = Theme::default();
        // Single content span: "Hello World" (11 chars)
        let mut lines = vec![make_line_with_gutter(vec![Span::raw("Hello World")])];

        // Select chars 2..5 ("llo") within the content
        let start = ContentPos {
            line: 0,
            char_offset: 2,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 5,
        };
        apply_selection_highlight(&mut lines, &start, &end, 80, &theme);

        // Gutter (1 span) + before "He" + selected "llo" + after " World" = 4 spans
        assert_eq!(
            lines[0].spans.len(),
            4,
            "expected 4 spans: gutter + before + selected + after"
        );
        assert_eq!(lines[0].spans[1].content, "He");
        assert_eq!(lines[0].spans[2].content, "llo");
        assert_eq!(lines[0].spans[2].style.bg, Some(theme.selection_bg));
        assert_eq!(lines[0].spans[3].content, " World");
        // Before and after should NOT have selection_bg
        assert_ne!(lines[0].spans[1].style.bg, Some(theme.selection_bg));
        assert_ne!(lines[0].spans[3].style.bg, Some(theme.selection_bg));
    }

    #[test]
    fn selection_highlight_across_span_boundary() {
        let theme = Theme::default();
        let style_a = Style::default().fg(Color::Red);
        let style_b = Style::default().fg(Color::Blue);
        // Two content spans: "Hello" (5 chars) + " World" (6 chars)
        let mut lines = vec![make_line_with_gutter(vec![
            Span::styled("Hello", style_a),
            Span::styled(" World", style_b),
        ])];

        // Select chars 3..8 → "lo" from first span + " Wor" from second
        let start = ContentPos {
            line: 0,
            char_offset: 3,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 8,
        };
        apply_selection_highlight(&mut lines, &start, &end, 80, &theme);

        // "Hello" spans chars 0..5, " World" spans chars 5..11
        // Selection 3..8 → "lo" (chars 3-4) from first, " Wo" (chars 5-7) from second
        // gutter + "Hel" + "lo" (selected) + " Wo" (selected) + "rld" = 5 spans
        assert_eq!(
            lines[0].spans.len(),
            5,
            "expected 5 spans, got: {:?}",
            lines[0]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
        assert_eq!(lines[0].spans[1].content, "Hel");
        assert_eq!(lines[0].spans[1].style.bg, None); // original style_a, no bg
        assert_eq!(lines[0].spans[2].content, "lo");
        assert_eq!(lines[0].spans[2].style.bg, Some(theme.selection_bg));
        assert_eq!(lines[0].spans[2].style.fg, Some(Color::Red)); // preserves fg
        assert_eq!(lines[0].spans[3].content, " Wo");
        assert_eq!(lines[0].spans[3].style.bg, Some(theme.selection_bg));
        assert_eq!(lines[0].spans[3].style.fg, Some(Color::Blue)); // preserves fg
        assert_eq!(lines[0].spans[4].content, "rld");
        assert_eq!(lines[0].spans[4].style.bg, None);
    }

    #[test]
    fn selection_highlight_full_span_no_split() {
        let theme = Theme::default();
        // Content span: "Hello" (5 chars)
        let mut lines = vec![make_line_with_gutter(vec![Span::raw("Hello")])];

        // Select entire span: chars 0..5
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 5,
        };
        apply_selection_highlight(&mut lines, &start, &end, 80, &theme);

        // gutter + single fully-highlighted span = 2 spans
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[1].content, "Hello");
        assert_eq!(lines[0].spans[1].style.bg, Some(theme.selection_bg));
    }

    #[test]
    fn selection_highlight_gutter_untouched() {
        let theme = Theme::default();
        let gutter_style = Style::default().fg(Color::DarkGray);
        let mut lines = vec![Line::from(vec![
            Span::styled(" · ", gutter_style),
            Span::raw("content here"),
        ])];

        // Select all content
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 100,
        };
        apply_selection_highlight(&mut lines, &start, &end, 80, &theme);

        // Gutter span should retain original style — no selection_bg
        assert_eq!(lines[0].spans[0].content, " · ");
        assert_eq!(lines[0].spans[0].style, gutter_style);
        assert_ne!(lines[0].spans[0].style.bg, Some(theme.selection_bg));
    }

    #[test]
    fn selection_highlight_multiline_highlights_middle_lines_fully() {
        let theme = Theme::default();
        let mut lines = vec![
            make_line_with_gutter(vec![Span::raw("first line")]),
            make_line_with_gutter(vec![Span::raw("middle line")]),
            make_line_with_gutter(vec![Span::raw("third line")]),
        ];

        // Select from char 6 on line 0 to char 5 on line 2
        // Middle line (line 1) should use line_end=usize::MAX, highlighting everything
        let start = ContentPos {
            line: 0,
            char_offset: 6,
        };
        let end = ContentPos {
            line: 2,
            char_offset: 5,
        };
        apply_selection_highlight(&mut lines, &start, &end, 80, &theme);

        // Line 0: "first " (no highlight) + "line" (highlighted)
        assert_eq!(lines[0].spans[1].content, "first ");
        assert_ne!(lines[0].spans[1].style.bg, Some(theme.selection_bg));
        assert_eq!(lines[0].spans[2].content, "line");
        assert_eq!(lines[0].spans[2].style.bg, Some(theme.selection_bg));

        // Line 1 (middle): entire content highlighted, no split
        assert_eq!(lines[1].spans[1].content, "middle line");
        assert_eq!(lines[1].spans[1].style.bg, Some(theme.selection_bg));

        // Line 2: "third" (highlighted) + " line" (no highlight)
        assert_eq!(lines[2].spans[1].content, "third");
        assert_eq!(lines[2].spans[1].style.bg, Some(theme.selection_bg));
        assert_eq!(lines[2].spans[2].content, " line");
        assert_ne!(lines[2].spans[2].style.bg, Some(theme.selection_bg));
    }

    #[test]
    fn selection_highlight_zero_width_does_nothing() {
        let theme = Theme::default();
        let mut lines = vec![make_line_with_gutter(vec![Span::raw("Hello")])];

        // Zero-width selection (start == end on same line) should skip
        let start = ContentPos {
            line: 0,
            char_offset: 3,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 3,
        };
        apply_selection_highlight(&mut lines, &start, &end, 80, &theme);

        // Should remain unchanged: gutter + original span
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[1].content, "Hello");
        assert_ne!(lines[0].spans[1].style.bg, Some(theme.selection_bg));
    }

    #[test]
    fn buffer_activity_spinner_with_elapsed() {
        let messages = vec![];
        let text = render_messages_to_string(
            60,
            10,
            &messages,
            Some((
                '⠋',
                "Thinking...".to_string(),
                false,
                Some(Duration::from_secs(42)),
            )),
        );
        assert!(text.contains("Thinking..."), "activity text should appear");
        assert!(
            text.contains("(42s)"),
            "elapsed should appear after activity, got:\n{text}"
        );
    }

    #[test]
    fn buffer_activity_spinner_minute_elapsed() {
        let messages = vec![];
        let text = render_messages_to_string(
            60,
            10,
            &messages,
            Some((
                '⠋',
                "Running read...".to_string(),
                false,
                Some(Duration::from_secs(83)),
            )),
        );
        assert!(
            text.contains("(1:23)"),
            "should show minute format, got:\n{text}"
        );
    }
}
