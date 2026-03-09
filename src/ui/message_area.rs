use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::message_block::{AssistantPart, CodeFence, DiffContent, DiffLine, MessageBlock, ToolGroup, ToolGroupStatus};
use super::theme::Theme;
use crate::tool::{IntentCategory, ToolName};

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
}

impl Default for MessageAreaState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            auto_scroll: true,
            content_height: 0,
            visible_height: 0,
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

/// Render structured message blocks into the given area.
pub fn render_message_blocks(
    frame: &mut Frame,
    area: Rect,
    messages: &[MessageBlock],
    state: &mut MessageAreaState,
    theme: &Theme,
    activity: Option<(char, String)>,
    context_pct: u8,
) {
    let mut lines: Vec<Line> = Vec::new();
    let available_width = area.width.max(1) as usize;

    // Pre-scan: identify which (msg_idx, part_idx) has the last code block
    let last_code_pos = find_last_code_block_position(messages);

    for (msg_idx, msg) in messages.iter().enumerate() {
        match msg {
            MessageBlock::User { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "│ ",
                            Style::default()
                                .fg(theme.user_msg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            text_line.to_string(),
                            Style::default().fg(theme.user_msg),
                        ),
                    ]));
                }
            }

            MessageBlock::Assistant {
                thinking,
                parts,
            } => {
                // Thinking block (collapsed by default)
                if let Some(t) = thinking {
                    if t.expanded {
                        lines.push(Line::from(Span::styled(
                            format!("\u{25bc} Thinking ({} tokens)", t.token_count),
                            Style::default()
                                .fg(theme.reasoning)
                                .add_modifier(Modifier::ITALIC),
                        )));
                        for content_line in t.content.lines() {
                            lines.push(Line::from(Span::styled(
                                format!("  {content_line}"),
                                Style::default().fg(theme.reasoning),
                            )));
                        }
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!("\u{25b6} Thinking ({} tokens)", t.token_count),
                            Style::default()
                                .fg(theme.reasoning)
                                .add_modifier(Modifier::ITALIC),
                        )));
                    }
                }

                // Parts in chronological order.
                // Track last-emitted intent to suppress repeated labels for
                // consecutive same-category tool groups (e.g. 3 reads in a row).
                // Text between groups resets tracking so the label reappears.
                let mut last_intent: Option<IntentCategory> = None;
                for (part_idx, part) in parts.iter().enumerate() {
                    match part {
                        AssistantPart::Text(text) => {
                            let show_copy_hint = last_code_pos == Some((msg_idx, part_idx));
                            render_text_with_code_blocks(text, &mut lines, theme, available_width, show_copy_hint);
                            last_intent = None;
                        }
                        AssistantPart::ToolGroup(group) => {
                            // Intent indicator — suppressed if same as previous group
                            if let Some(category) = infer_group_intent(group) {
                                if last_intent != Some(category) {
                                    lines.push(render_intent_line(category, available_width, theme));
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
                                        ToolGroupStatus::Running { .. } => " running...".to_string(),
                                        ToolGroupStatus::Complete => String::new(),
                                    },
                                };

                                let color = if call.is_error {
                                    theme.error
                                } else {
                                    match call.tool_name {
                                        ToolName::Read | ToolName::Grep | ToolName::Glob
                                        | ToolName::List | ToolName::Webfetch => theme.tool_read,
                                        ToolName::Edit | ToolName::Write | ToolName::Patch
                                        | ToolName::Move | ToolName::Copy | ToolName::Delete
                                        | ToolName::Mkdir | ToolName::Memory => theme.tool_write,
                                        ToolName::Bash | ToolName::Question | ToolName::Todo => theme.accent,
                                    }
                                };

                                lines.push(Line::from(Span::styled(
                                    format!(
                                        "{status_indicator} {} {}({}){}",
                                        call.tool_name.tool_marker(),
                                        call.tool_name, call.args_summary, result_part
                                    ),
                                    Style::default().fg(color),
                                )));

                                // Expanded output — diff content or raw output fallback
                                if call.expanded {
                                    if let Some(diff) = &call.diff_content {
                                        render_diff_lines(&mut lines, diff, call.result_summary.as_deref(), theme, context_pct);
                                    } else if let Some(output) = &call.full_output {
                                        for output_line in output.lines() {
                                            lines.push(Line::from(Span::styled(
                                                format!("  {output_line}"),
                                                Style::default().fg(theme.dim),
                                            )));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            MessageBlock::System { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default()
                            .fg(theme.system_msg)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }

            MessageBlock::Error { text } => {
                for text_line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.error),
                    )));
                }
            }

            MessageBlock::Permission {
                tool_name,
                args_summary,
                diff_content,
            } => {
                // Top rule
                lines.push(Line::from(Span::styled(
                    "\u{2500}".repeat(40),
                    Style::default().fg(theme.permission),
                )));
                // Prompt line
                lines.push(Line::from(vec![
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
                ]));
                // Inline diff preview if available
                if let Some(diff) = diff_content {
                    render_diff_lines(&mut lines, diff, None, theme, context_pct);
                }
                // Options line with highlighted key letters
                lines.push(Line::from(vec![
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
                ]));
                // Bottom rule
                lines.push(Line::from(Span::styled(
                    "\u{2500}".repeat(40),
                    Style::default().fg(theme.permission),
                )));
            }
        }

        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Inline activity spinner (replaces the old "..." and status bar spinner)
    if let Some((spinner, text)) = activity {
        lines.push(Line::from(Span::styled(
            format!("{spinner} {text}"),
            Style::default().fg(theme.accent),
        )));
        lines.push(Line::from(""));
    }

    // Compute content height with wrapping
    let content_height_u32: u32 = lines
        .iter()
        .map(|line| {
            let line_width: usize = line.width();
            if line_width == 0 {
                1u32
            } else {
                ((line_width + available_width - 1) / available_width) as u32
            }
        })
        .sum();
    let content_height = content_height_u32.min(u16::MAX as u32) as u16;
    let visible_height = area.height.saturating_sub(2);

    state.update_dimensions(content_height, visible_height);

    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().fg(theme.fg));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));

    frame.render_widget(paragraph, area);
}

/// Render diff content into styled lines with box-drawing frame.
fn render_diff_lines(
    lines: &mut Vec<Line<'_>>,
    diff: &DiffContent,
    result_summary: Option<&str>,
    theme: &Theme,
    context_pct: u8,
) {
    match diff {
        DiffContent::EditDiff { lines: diff_lines }
        | DiffContent::PatchDiff { lines: diff_lines } => {
            // Top border
            lines.push(Line::from(Span::styled(
                "  \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                Style::default().fg(theme.border_color(context_pct)),
            )));

            for diff_line in diff_lines {
                let (prefix, text, color) = match diff_line {
                    DiffLine::Removal(t) => ("-", t.as_str(), theme.error),
                    DiffLine::Addition(t) => ("+", t.as_str(), theme.success),
                    DiffLine::Context(t) => (" ", t.as_str(), theme.dim),
                    DiffLine::HunkHeader(t) => ("", t.as_str(), theme.dim),
                };
                lines.push(Line::from(vec![
                    Span::styled("  \u{2502} ", Style::default().fg(theme.border_color(context_pct))),
                    Span::styled(
                        format!("{prefix}{text}"),
                        Style::default().fg(color),
                    ),
                ]));
            }

            // Bottom border
            lines.push(Line::from(Span::styled(
                "  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                Style::default().fg(theme.border_color(context_pct)),
            )));
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
/// (question/todo).
///
/// Priority: editing > executing > exploring. When a group contains
/// mixed tools, the highest-priority wins (mutations matter most).
fn infer_group_intent(group: &ToolGroup) -> Option<IntentCategory> {
    let mut has_exploring = false;
    let mut has_editing = false;
    let mut has_executing = false;

    for call in &group.calls {
        match call.tool_name.intent_category() {
            IntentCategory::Exploring => has_exploring = true,
            IntentCategory::Editing => has_editing = true,
            IntentCategory::Executing => has_executing = true,
            IntentCategory::Asking => {} // doesn't influence the label
        }
    }

    if has_editing {
        Some(IntentCategory::Editing)
    } else if has_executing {
        Some(IntentCategory::Executing)
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
        IntentCategory::Asking => ("asking", theme.dim),
    };

    let prefix = format!("\u{2500}\u{2500} {label} ");
    let prefix_chars = prefix.chars().count();
    let dash_count = width.saturating_sub(prefix_chars);
    let dashes = "\u{2500}".repeat(dash_count);
    let full = format!("{prefix}{dashes}");

    Line::from(Span::styled(full, Style::default().fg(color)))
}

/// Find the `(msg_idx, part_idx)` of the last assistant text part that contains
/// a code block. Scans messages backward, mirroring `extract_last_code_block()`
/// in app.rs. Returns `None` if no code blocks exist.
fn find_last_code_block_position(messages: &[MessageBlock]) -> Option<(usize, usize)> {
    for (msg_idx, msg) in messages.iter().enumerate().rev() {
        if let MessageBlock::Assistant { parts, .. } = msg {
            for (part_idx, part) in parts.iter().enumerate().rev() {
                if let AssistantPart::Text(text) = part {
                    let mut in_code_block = false;
                    let mut found = false;
                    for line in text.lines() {
                        match CodeFence::classify(line, in_code_block) {
                            CodeFence::Open { .. } => {
                                in_code_block = true;
                                found = true;
                            }
                            CodeFence::Close => {
                                in_code_block = false;
                            }
                            CodeFence::NotFence => {}
                        }
                    }
                    if found {
                        return Some((msg_idx, part_idx));
                    }
                }
            }
        }
    }
    None
}

/// Tracks the last code block header position within a `render_text_with_code_blocks` call
/// so we can post-patch it with the copy hint.
enum LastHeaderInfo {
    /// A rendered header line (language label) — index into the `lines` vec.
    Rendered(usize),
    /// A bare fence (no language) that produced no header — insertion point in `lines` vec.
    BareAt(usize),
}

/// Detect fenced code blocks in assistant text and render with tinted background.
///
/// Uses a stateless line-by-line scanner: lines starting with ` ``` ` (≤3 leading
/// spaces) toggle code block mode. Opening fences emit a header line with optional
/// language label; closing fences are consumed. Code lines get `code_bg` background.
///
/// When `show_copy_hint` is true, the last code block's header line gets a
/// right-aligned copy hint (dim text on code_bg) to indicate the copy-to-clipboard
/// keybinding. Bare fences that normally produce no header get a minimal header
/// inserted just for the hint.
fn render_text_with_code_blocks(
    text: &str,
    lines: &mut Vec<Line<'_>>,
    theme: &Theme,
    available_width: usize,
    show_copy_hint: bool,
) {
    let mut in_code_block = false;
    let mut last_header: Option<LastHeaderInfo> = None;

    for text_line in text.lines() {
        match CodeFence::classify(text_line, in_code_block) {
            CodeFence::Open { lang } => {
                let code_bg_style = Style::default().fg(theme.dim).bg(theme.code_bg);

                if !lang.is_empty() {
                    // Language label followed by space fill (background tint provides framing)
                    let label = format!("{lang} ");
                    let fill_len = available_width.saturating_sub(label.chars().count());
                    let fill = " ".repeat(fill_len);
                    let header_idx = lines.len();
                    lines.push(
                        Line::from(vec![
                            Span::styled(label, code_bg_style),
                            Span::styled(fill, code_bg_style),
                        ])
                        .style(Style::default().bg(theme.code_bg)),
                    );
                    if show_copy_hint {
                        last_header = Some(LastHeaderInfo::Rendered(header_idx));
                    }
                } else if show_copy_hint {
                    // Bare fence — record insertion point for potential minimal header
                    last_header = Some(LastHeaderInfo::BareAt(lines.len()));
                }
                // No language without hint: skip header entirely — code_bg on code lines
                // provides framing. An all-space header would be invisible.
                in_code_block = true;
            }
            CodeFence::Close => {
                in_code_block = false;
            }
            CodeFence::NotFence if in_code_block => {
                // Code line — tinted background
                lines.push(
                    Line::from(Span::styled(
                        text_line.to_string(),
                        Style::default().fg(theme.assistant_msg).bg(theme.code_bg),
                    ))
                    .style(Style::default().bg(theme.code_bg)),
                );
            }
            CodeFence::NotFence => {
                // Normal prose line
                lines.push(Line::from(Span::styled(
                    text_line.to_string(),
                    Style::default().fg(theme.assistant_msg),
                )));
            }
        }
    }

    // Post-patch: add right-aligned copy hint to the last code block's header
    if show_copy_hint {
        if let Some(header_info) = last_header {
            let code_bg_style = Style::default().fg(theme.dim).bg(theme.code_bg);
            let hint = "(press ctrl-y to copy)";
            let hint_len = hint.len(); // all ASCII

            match header_info {
                LastHeaderInfo::Rendered(idx) => {
                    // Replace the existing header line with one that includes the hint.
                    // Original: [label_span, fill_span] — we rebuild with label + gap + hint.
                    let existing = &lines[idx];
                    let label_text: String = existing.spans.first()
                        .map(|s| s.content.as_ref().to_string())
                        .unwrap_or_default();
                    let label_len = label_text.chars().count();
                    let gap_len = available_width.saturating_sub(label_len + hint_len);
                    let gap = " ".repeat(gap_len);
                    lines[idx] = Line::from(vec![
                        Span::styled(label_text, code_bg_style),
                        Span::styled(gap, code_bg_style),
                        Span::styled(hint.to_string(), code_bg_style),
                    ])
                    .style(Style::default().bg(theme.code_bg));
                }
                LastHeaderInfo::BareAt(idx) => {
                    // Insert a minimal header with just the copy hint right-aligned.
                    let gap_len = available_width.saturating_sub(hint_len);
                    let gap = " ".repeat(gap_len);
                    lines.insert(
                        idx,
                        Line::from(vec![
                            Span::styled(gap, code_bg_style),
                            Span::styled(hint.to_string(), code_bg_style),
                        ])
                        .style(Style::default().bg(theme.code_bg)),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // -- render_diff_lines tests --

    use super::super::message_block::{DiffContent, DiffLine};
    use super::super::theme::Theme;

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
        render_diff_lines(&mut output, &diff, None, &theme, 0);
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
        render_diff_lines(&mut output, &diff, None, &theme, 0);
        // top border + 4 diff lines + bottom border = 6 lines
        assert_eq!(output.len(), 6);
    }

    #[test]
    fn render_diff_lines_write_summary_created_verb() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 10 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, Some("Created /tmp/foo (42 bytes)"), &theme, 0);
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
        render_diff_lines(&mut output, &diff, Some("Overwrote /tmp/foo (20 bytes)"), &theme, 0);
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(text.contains("Overwrote"), "verb should be Overwrote");
    }

    #[test]
    fn render_diff_lines_write_summary_defaults_to_overwrote() {
        let theme = Theme::default();
        let diff = DiffContent::WriteSummary { line_count: 3 };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme, 0);
        assert_eq!(output.len(), 1);
        let text = format!("{:?}", output[0]);
        assert!(text.contains("Overwrote"), "should default to Overwrote when no summary");
    }

    #[test]
    fn render_diff_lines_empty_edit_diff() {
        let theme = Theme::default();
        let diff = DiffContent::EditDiff { lines: vec![] };
        let mut output: Vec<Line> = Vec::new();
        render_diff_lines(&mut output, &diff, None, &theme, 0);
        // top border + 0 diff lines + bottom border = 2 lines
        assert_eq!(output.len(), 2);
    }

    // -- Buffer rendering tests --
    //
    // These test the actual render pipeline through ratatui's TestBackend,
    // catching layout bugs that pure data-model tests miss.

    use ratatui::layout::Rect;
    use super::super::message_block::{ThinkingBlock, ToolCall, ToolGroup, ToolGroupStatus};

    /// Helper: render message blocks into a buffer and return the buffer text as a single string.
    fn render_messages_to_string(
        width: u16,
        height: u16,
        messages: &[MessageBlock],
        activity: Option<(char, String)>,
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
        assert!(text.contains("│ Hello world"), "user message should have '│ ' prefix, got:\n{text}");
    }

    #[test]
    fn buffer_assistant_text_rendered() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text("Response text here".to_string())],
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("Response text here"), "assistant text should appear, got:\n{text}");
        // Should NOT have "│ " prefix
        assert!(!text.contains("│ Response"), "assistant should not have user prefix");
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
        assert!(text.contains("\u{25b6}"), "collapsed thinking should show ▶");
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
        assert!(text.contains("my thoughts"), "expanded thinking should show content");
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
                }],
                status: ToolGroupStatus::Preparing,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(text.contains("preparing..."), "preparing tool should show 'preparing...'");
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
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(text.contains("\u{25b6}"), "collapsed complete should show ▶");
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
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 15, &messages, None);
        // Box-drawing frame
        assert!(text.contains("\u{250c}"), "should have top-left corner ┌");
        assert!(text.contains("\u{2514}"), "should have bottom-left corner └");
        assert!(text.contains("-old line"), "should show removal with -");
        assert!(text.contains("+new line"), "should show addition with +");
    }

    #[test]
    fn buffer_system_message_rendered() {
        let messages = vec![MessageBlock::System {
            text: "System notice".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("System notice"), "system message should appear");
    }

    #[test]
    fn buffer_error_message_rendered() {
        let messages = vec![MessageBlock::Error {
            text: "Something broke".to_string(),
        }];
        let text = render_messages_to_string(60, 10, &messages, None);
        assert!(text.contains("Something broke"), "error message should appear");
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
        let text = render_messages_to_string(60, 10, &messages, Some(('⠋', "Thinking...".to_string())));
        assert!(text.contains("Thinking..."), "activity text should appear");
    }

    #[test]
    fn buffer_blank_line_between_messages() {
        let messages = vec![
            MessageBlock::User { text: "msg1".to_string() },
            MessageBlock::User { text: "msg2".to_string() },
        ];
        let text = render_messages_to_string(60, 10, &messages, None);
        // Find positions — msg2 should not immediately follow msg1
        let pos1 = text.find("│ msg1").expect("msg1 not found");
        let pos2 = text.find("│ msg2").expect("msg2 not found");
        // There should be at least one blank line between them (newline + spaces + newline)
        let between = &text[pos1..pos2];
        let line_count = between.lines().count();
        assert!(line_count >= 2, "should have blank line separation, got {line_count} lines between messages");
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
        for tool in [ToolName::Read, ToolName::Grep, ToolName::Glob, ToolName::List, ToolName::Webfetch] {
            let group = make_tool_group(&[tool]);
            assert_eq!(infer_group_intent(&group), Some(IntentCategory::Exploring), "{tool} should produce Exploring");
        }
    }

    #[test]
    fn infer_group_intent_write_tools() {
        for tool in [ToolName::Edit, ToolName::Write, ToolName::Patch, ToolName::Memory] {
            let group = make_tool_group(&[tool]);
            assert_eq!(infer_group_intent(&group), Some(IntentCategory::Editing), "{tool} should produce Editing");
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
        assert_eq!(infer_group_intent(&group), Some(IntentCategory::Editing), "editing takes priority over exploring");
    }

    #[test]
    fn infer_group_intent_mixed_read_bash() {
        let group = make_tool_group(&[ToolName::Read, ToolName::Bash]);
        assert_eq!(infer_group_intent(&group), Some(IntentCategory::Executing), "executing takes priority over exploring");
    }

    #[test]
    fn infer_group_intent_mixed_edit_bash() {
        let group = make_tool_group(&[ToolName::Edit, ToolName::Bash]);
        assert_eq!(infer_group_intent(&group), Some(IntentCategory::Editing), "editing takes priority over executing");
    }

    #[test]
    fn infer_group_intent_only_asking_tools() {
        let group = make_tool_group(&[ToolName::Question, ToolName::Todo]);
        assert_eq!(infer_group_intent(&group), None, "asking tools should not produce a label");
    }

    #[test]
    fn infer_group_intent_asking_plus_exploring() {
        let group = make_tool_group(&[ToolName::Todo, ToolName::Read]);
        assert_eq!(infer_group_intent(&group), Some(IntentCategory::Exploring), "exploring should show even with asking tools");
    }

    #[test]
    fn infer_group_intent_all_categories() {
        let group = make_tool_group(&[ToolName::Read, ToolName::Edit, ToolName::Bash, ToolName::Question]);
        assert_eq!(infer_group_intent(&group), Some(IntentCategory::Editing), "editing has highest priority");
    }

    // -- render_intent_line tests --

    #[test]
    fn render_intent_line_format() {
        let theme = Theme::default();
        let line = render_intent_line(IntentCategory::Exploring, 40, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("\u{2500}\u{2500} exploring "), "should start with '── exploring '");
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
    fn render_intent_line_narrow_width() {
        let theme = Theme::default();
        // Width exactly equal to prefix — no trailing dashes
        let prefix_len = "\u{2500}\u{2500} exploring ".chars().count(); // 13 chars
        let line = render_intent_line(IntentCategory::Exploring, prefix_len, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "\u{2500}\u{2500} exploring ", "at exact prefix width, no dashes appended");
        assert_eq!(text.chars().count(), prefix_len, "should be exactly prefix length");

        // Width smaller than prefix — no dashes, no panic
        let line2 = render_intent_line(IntentCategory::Exploring, 5, &theme);
        let text2: String = line2.spans.iter().map(|s| s.content.as_ref()).collect();
        // Still contains the full prefix (saturating_sub produces 0 dashes)
        assert!(text2.starts_with("\u{2500}\u{2500} exploring "), "prefix always rendered");
        assert_eq!(text2.chars().count(), prefix_len, "output is prefix-length when width < prefix");
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
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        assert!(text.contains("exploring"), "should have exploring label for read group");
        assert!(text.contains("editing"), "should have editing label for edit group");
        // exploring should appear before editing in the output
        let pos_exploring = text.find("exploring").unwrap();
        let pos_editing = text.find("editing").unwrap();
        assert!(pos_exploring < pos_editing, "exploring should come before editing");
    }

    #[test]
    fn buffer_intent_no_label_for_text_only() {
        let messages = vec![MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text("Just a text response.".into())],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(!text.contains("exploring"), "no intent label for text-only response");
        assert!(!text.contains("editing"), "no intent label for text-only response");
        assert!(!text.contains("executing"), "no intent label for text-only response");
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
                }],
                status: ToolGroupStatus::Complete,
            })],
        }];
        let text = render_messages_to_string(80, 10, &messages, None);
        assert!(!text.contains("exploring"), "no exploring label for question tool");
        assert!(!text.contains("editing"), "no editing label for question tool");
        assert!(!text.contains("executing"), "no executing label for question tool");
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
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        let count = text.matches("exploring").count();
        assert_eq!(count, 2, "expected 2 'exploring' labels (text resets dedup) but found {count}");
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
                    }],
                    status: ToolGroupStatus::Complete,
                }),
            ],
        }];
        let text = render_messages_to_string(80, 20, &messages, None);
        let count = text.matches("exploring").count();
        assert_eq!(count, 2, "expected 2 'exploring' labels (asking resets dedup) but found {count}");
    }

    // -- render_text_with_code_blocks tests --

    #[test]
    fn code_block_renders_with_header() {
        let theme = Theme::default();
        let text = "before\n```rust\nfn main() {}\n```\nafter";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        // 5 input lines → "before", header, "fn main() {}", (closing consumed), "after" = 4 output lines
        assert_eq!(lines.len(), 4, "expected 4 lines, got {}", lines.len());
        // Header should contain language label
        let header_text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header_text.starts_with("rust "), "header should start with 'rust ', got: {header_text}");
        // Fill after label should be all spaces (copy-text constraint: no box-drawing chars)
        assert!(header_text["rust ".len()..].chars().all(|c| c == ' '),
            "header fill should be all spaces, got: {header_text}");
    }

    #[test]
    fn code_block_no_language_skips_header() {
        let theme = Theme::default();
        let text = "```\ncode\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 30, false);
        // No header for bare fences — just the code line (closing consumed)
        assert_eq!(lines.len(), 1);
        let code_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(code_text, "code");
        assert_eq!(lines[0].style.bg, Some(theme.code_bg), "code line should have code_bg");
    }

    #[test]
    fn unclosed_code_block_tints_remaining() {
        let theme = Theme::default();
        let text = "before\n```python\nline1\nline2";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        // "before", header, "line1", "line2" = 4 lines
        assert_eq!(lines.len(), 4);
        // Lines 2 and 3 (code lines) should have code_bg background on Line.style
        for i in 2..4 {
            assert_eq!(
                lines[i].style.bg, Some(theme.code_bg),
                "unclosed code line {i} should have code_bg"
            );
        }
    }

    #[test]
    fn empty_code_block() {
        let theme = Theme::default();
        let text = "```\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 20, false);
        // No header for bare fences, no code content — nothing to render
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn inline_backticks_not_treated_as_fence() {
        let theme = Theme::default();
        let text = "use `foo` and ``bar``";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        assert_eq!(lines.len(), 1);
        // Should have no code_bg
        assert_eq!(lines[0].style.bg, None, "inline backticks should not trigger code block");
    }

    #[test]
    fn multiple_code_blocks() {
        let theme = Theme::default();
        let text = "text1\n```rust\nfn a() {}\n```\ntext2\n```go\nfunc b() {}\n```\ntext3";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        // text1, header1, "fn a() {}", text2, header2, "func b() {}", text3 = 7 lines
        assert_eq!(lines.len(), 7, "expected 7 lines, got {}", lines.len());
        // Normal text lines should NOT have code_bg
        assert_eq!(lines[0].style.bg, None, "text1 should not have bg");
        assert_eq!(lines[3].style.bg, None, "text2 should not have bg");
        assert_eq!(lines[6].style.bg, None, "text3 should not have bg");
        // Code lines should have code_bg
        assert_eq!(lines[2].style.bg, Some(theme.code_bg), "code line 1 should have bg");
        assert_eq!(lines[5].style.bg, Some(theme.code_bg), "code line 2 should have bg");
    }

    #[test]
    fn deeply_indented_fence_ignored() {
        let theme = Theme::default();
        let text = "    ```rust\nstill normal";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        // 4 spaces = not a fence, both lines rendered as normal text
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].style.bg, None, "4-space indented fence should be normal text");
        assert_eq!(lines[1].style.bg, None, "following line should be normal text");
    }

    #[test]
    fn code_block_header_has_bg() {
        let theme = Theme::default();
        let text = "```js\nconsole.log();\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        // Header line should have code_bg on Line.style
        assert_eq!(
            lines[0].style.bg,
            Some(theme.code_bg),
            "header line should have code_bg background"
        );
    }

    #[test]
    fn fence_closes_code_block() {
        let theme = Theme::default();
        let text = "```\ncode\n```\nafter";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        // No header for bare fence, "code" + "after" = 2 lines (closing fence consumed)
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].style.bg, Some(theme.code_bg), "code line should have code_bg");
        assert_eq!(lines[1].style.bg, None, "line after closing fence should be normal text");
    }

    #[test]
    fn tab_indented_fence_ignored() {
        let theme = Theme::default();
        let text = "\t```rust\nstill normal";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        // Tab is not a space — fence should not be recognized
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].style.bg, None, "tab-indented fence should be normal text");
        assert_eq!(lines[1].style.bg, None, "following line should be normal text");
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
        assert!(text.contains("rust"), "should contain language label 'rust'");
        assert!(!text.contains('\u{2500}'), "should not contain ─ (copy-text constraint)");
        // The code line should appear
        assert!(text.contains("fn main() {}"), "should contain code content");
        // The fence lines (```) should NOT appear
        assert!(!text.contains("```"), "fence markers should be consumed, not rendered");
        // Normal text should appear
        assert!(text.contains("Here is code:"), "text before block should appear");
        assert!(text.contains("Done."), "text after block should appear");
    }

    // -- find_last_code_block_position tests --

    #[test]
    fn find_last_code_block_position_empty_messages() {
        assert_eq!(find_last_code_block_position(&[]), None);
    }

    #[test]
    fn find_last_code_block_position_no_code_blocks() {
        let messages = vec![
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text("no code here".into())],
            },
        ];
        assert_eq!(find_last_code_block_position(&messages), None);
    }

    #[test]
    fn find_last_code_block_position_single_assistant() {
        let messages = vec![
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text("```rust\nfn main() {}\n```".into())],
            },
        ];
        assert_eq!(find_last_code_block_position(&messages), Some((0, 0)));
    }

    #[test]
    fn find_last_code_block_position_multiple_messages() {
        let messages = vec![
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text("```\ncode1\n```".into())],
            },
            MessageBlock::User { text: "next".into() },
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text("```\ncode2\n```".into())],
            },
        ];
        assert_eq!(find_last_code_block_position(&messages), Some((2, 0)));
    }

    #[test]
    fn find_last_code_block_position_multiple_parts() {
        let messages = vec![
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![
                    AssistantPart::Text("```\nfirst\n```".into()),
                    AssistantPart::ToolGroup(ToolGroup {
                        calls: vec![],
                        status: ToolGroupStatus::Complete,
                    }),
                    AssistantPart::Text("```\nsecond\n```".into()),
                ],
            },
        ];
        assert_eq!(find_last_code_block_position(&messages), Some((0, 2)));
    }

    #[test]
    fn find_last_code_block_position_skips_non_assistant() {
        let messages = vec![
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text("```\ncode\n```".into())],
            },
            MessageBlock::User { text: "```\nnot counted\n```".into() },
            MessageBlock::System { text: "```\nalso not counted\n```".into() },
        ];
        assert_eq!(find_last_code_block_position(&messages), Some((0, 0)));
    }

    #[test]
    fn find_last_code_block_position_unclosed_block() {
        let messages = vec![
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text("```python\nstill typing...".into())],
            },
        ];
        assert_eq!(find_last_code_block_position(&messages), Some((0, 0)));
    }

    // -- render_text_with_code_blocks copy hint tests --

    /// The hint text used in copy hint assertions.
    const COPY_HINT: &str = "(press ctrl-y to copy)";

    #[test]
    fn copy_hint_with_language_label() {
        let theme = Theme::default();
        let text = "```rust\nfn main() {}\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, true);
        // Header + code line = 2 lines
        assert_eq!(lines.len(), 2);
        let header_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header_text.starts_with("rust "), "header should start with lang label");
        assert!(header_text.ends_with(COPY_HINT), "header should end with copy hint, got: {header_text}");
    }

    #[test]
    fn copy_hint_bare_fence_gets_minimal_header() {
        let theme = Theme::default();
        let text = "```\ncode\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, true);
        // Minimal header inserted + code line = 2 lines (vs 1 without hint)
        assert_eq!(lines.len(), 2, "bare fence with hint should get a header line");
        let header_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header_text.ends_with(COPY_HINT), "minimal header should end with copy hint, got: {header_text}");
        assert_eq!(header_text.chars().count(), 40, "header should fill available width");
    }

    #[test]
    fn copy_hint_only_on_last_block() {
        let theme = Theme::default();
        let text = "```rust\nfirst\n```\n```go\nsecond\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 60, true);
        // header1 + code1 + header2 + code2 = 4 lines
        assert_eq!(lines.len(), 4);
        let header1: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let header2: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!header1.contains(COPY_HINT), "first block header should NOT have copy hint");
        assert!(header2.ends_with(COPY_HINT), "last block header should have copy hint");
    }

    #[test]
    fn copy_hint_false_no_hint() {
        let theme = Theme::default();
        let text = "```rust\ncode\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, false);
        let header_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!header_text.contains(COPY_HINT), "show_copy_hint=false should not add hint");
    }

    #[test]
    fn copy_hint_no_code_blocks_no_crash() {
        let theme = Theme::default();
        let text = "just plain text, no fences";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, true);
        assert_eq!(lines.len(), 1);
        let line_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!line_text.contains(COPY_HINT), "no code blocks means no copy hint");
    }

    #[test]
    fn copy_hint_bare_last_language_first() {
        let theme = Theme::default();
        // First block has language, second is bare — hint goes on bare (last)
        let text = "```rust\nfirst\n```\n```\nsecond\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, true);
        // header1 + code1 + bare_header + code2 = 4 lines
        assert_eq!(lines.len(), 4, "expected 4 lines, got {}", lines.len());
        let header1: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let bare_header: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!header1.contains(COPY_HINT), "first header should not have copy hint");
        assert!(bare_header.ends_with(COPY_HINT), "bare last header should have copy hint");
    }

    #[test]
    fn copy_hint_language_last_bare_first() {
        let theme = Theme::default();
        // First block is bare, second has language — hint goes on language (last)
        let text = "```\nfirst\n```\n```rust\nsecond\n```";
        let mut lines: Vec<Line> = Vec::new();
        render_text_with_code_blocks(text, &mut lines, &theme, 40, true);
        // code1 (bare, no header since not last) + header2 + code2 = 3 lines
        assert_eq!(lines.len(), 3, "expected 3 lines, got {}", lines.len());
        let header: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.ends_with(COPY_HINT), "last block's lang header should have copy hint");
    }

    // -- Integration test: full render pipeline --

    #[test]
    fn buffer_code_block_copy_hint_visible() {
        let messages = vec![
            MessageBlock::User { text: "Show me code".into() },
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::Text(
                    "Here:\n```rust\nfn main() {}\n```".to_string(),
                )],
            },
        ];
        let text = render_messages_to_string(60, 15, &messages, None);
        // Copy hint should appear exactly once in the rendered output
        let count = text.matches("ctrl-y to copy").count();
        assert_eq!(count, 1, "expected exactly 1 copy hint, found {count} in:\n{text}");
    }
}
