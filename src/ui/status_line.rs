//! Status line state and rendering for the TUI footer.

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::tool::ToolName;

use super::input::AgentMode;
use super::theme::Theme;

/// Braille spinner frames, cycled on each 100ms tick.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];

/// Current activity shown in the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// No activity — agent is idle.
    Idle,
    /// LLM is generating text (streaming, no tool calls yet).
    Thinking,
    /// A tool is currently executing.
    RunningTool {
        tool_name: ToolName,
        args_summary: String,
    },
    /// Waiting for the user to approve a permission prompt.
    WaitingForPermission,
    /// Compaction is in progress.
    Compacting,
}

/// State for the status line footer.
pub struct StatusLineState {
    /// Current activity.
    pub activity: Activity,
    /// Spinner frame index (0..SPINNER_FRAMES.len()), advanced on tick.
    pub spinner_frame: usize,
    /// Model reference string (e.g., "gpt-4o").
    pub model_name: String,
    /// Total tokens used in this session.
    pub total_tokens: u64,
    /// Context window size for the current model.
    pub context_window: u64,
}

impl Default for StatusLineState {
    fn default() -> Self {
        Self {
            activity: Activity::Idle,
            spinner_frame: 0,
            model_name: String::new(),
            total_tokens: 0,
            context_window: 0,
        }
    }
}

impl StatusLineState {
    /// Advance the spinner to the next frame. Called on each tick.
    pub fn tick(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
    }

    /// Get the current spinner character, or None if idle.
    pub fn spinner_char(&self) -> Option<char> {
        if self.activity == Activity::Idle {
            None
        } else {
            Some(SPINNER_FRAMES[self.spinner_frame])
        }
    }

    /// Format the activity as a display string.
    pub fn activity_text(&self) -> String {
        match &self.activity {
            Activity::Idle => String::new(),
            Activity::Thinking => "Thinking...".to_string(),
            Activity::RunningTool {
                tool_name,
                args_summary,
            } => {
                if args_summary.is_empty() {
                    format!("Running {}...", tool_name)
                } else {
                    format!("Running {}({})...", tool_name, args_summary)
                }
            }
            Activity::WaitingForPermission => "Waiting for permission...".to_string(),
            Activity::Compacting => "Compacting...".to_string(),
        }
    }

    /// Context window usage as a percentage (0–100).
    pub fn context_usage_pct(&self) -> u8 {
        if self.context_window == 0 {
            0
        } else {
            ((self.total_tokens as f64 / self.context_window as f64) * 100.0).min(100.0) as u8
        }
    }
}

/// Format a token count with K/M suffixes.
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Render the status line into the given 1-row area.
pub fn render_status_line(
    frame: &mut Frame,
    area: Rect,
    state: &StatusLineState,
    theme: &Theme,
    mode: AgentMode,
) {
    let mut left_spans: Vec<Span> = Vec::new();

    // Spinner + activity text
    if let Some(spinner) = state.spinner_char() {
        left_spans.push(Span::styled(
            format!("{spinner} "),
            Style::default().fg(theme.accent),
        ));
    }
    let activity = state.activity_text();
    if !activity.is_empty() {
        left_spans.push(Span::styled(
            activity,
            Style::default().fg(theme.accent),
        ));
    }

    // Right side: model | tokens/context (pct%) | mode
    let mut right_parts: Vec<String> = Vec::new();

    if !state.model_name.is_empty() {
        right_parts.push(state.model_name.clone());
    }

    if state.context_window > 0 {
        let pct = state.context_usage_pct();
        right_parts.push(format!(
            "{}/{} ({}%)",
            format_tokens(state.total_tokens),
            format_tokens(state.context_window),
            pct,
        ));
    } else if state.total_tokens > 0 {
        right_parts.push(format_tokens(state.total_tokens));
    }

    right_parts.push(mode.display_name().to_string());

    let right_text = right_parts.join(" \u{2502} ");
    let pct = state.context_usage_pct();
    let right_color = if pct >= 80 {
        theme.error
    } else if pct >= 50 {
        theme.warning
    } else {
        theme.dim
    };

    // Calculate padding
    let left_width: usize = left_spans.iter().map(|s| s.width()).sum();
    let right_width = right_text.chars().count();
    let available = area.width as usize;
    let padding = available.saturating_sub(left_width + right_width);

    left_spans.push(Span::raw(" ".repeat(padding)));
    left_spans.push(Span::styled(right_text, Style::default().fg(right_color)));

    let line = Line::from(left_spans);
    let block = Block::default().borders(Borders::NONE);
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_idle() {
        let state = StatusLineState::default();
        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.spinner_frame, 0);
        assert!(state.model_name.is_empty());
    }

    #[test]
    fn tick_advances_spinner() {
        let mut state = StatusLineState::default();
        state.activity = Activity::Thinking;
        assert_eq!(state.spinner_frame, 0);
        state.tick();
        assert_eq!(state.spinner_frame, 1);
        // Wraps around
        for _ in 0..7 {
            state.tick();
        }
        assert_eq!(state.spinner_frame, 0);
    }

    #[test]
    fn spinner_char_none_when_idle() {
        let state = StatusLineState::default();
        assert_eq!(state.spinner_char(), None);
    }

    #[test]
    fn spinner_char_some_when_active() {
        let mut state = StatusLineState::default();
        state.activity = Activity::Thinking;
        assert_eq!(state.spinner_char(), Some('⠋'));
        state.tick();
        assert_eq!(state.spinner_char(), Some('⠙'));
    }

    #[test]
    fn activity_text_variants() {
        assert_eq!(
            StatusLineState {
                activity: Activity::Idle,
                ..Default::default()
            }
            .activity_text(),
            ""
        );
        assert_eq!(
            StatusLineState {
                activity: Activity::Thinking,
                ..Default::default()
            }
            .activity_text(),
            "Thinking..."
        );
        assert_eq!(
            StatusLineState {
                activity: Activity::RunningTool {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".into(),
                },
                ..Default::default()
            }
            .activity_text(),
            "Running read(src/main.rs)..."
        );
        assert_eq!(
            StatusLineState {
                activity: Activity::RunningTool {
                    tool_name: ToolName::Bash,
                    args_summary: String::new(),
                },
                ..Default::default()
            }
            .activity_text(),
            "Running bash..."
        );
        assert_eq!(
            StatusLineState {
                activity: Activity::WaitingForPermission,
                ..Default::default()
            }
            .activity_text(),
            "Waiting for permission..."
        );
        assert_eq!(
            StatusLineState {
                activity: Activity::Compacting,
                ..Default::default()
            }
            .activity_text(),
            "Compacting..."
        );
    }

    #[test]
    fn context_usage_pct_calculation() {
        let state = StatusLineState {
            total_tokens: 12800,
            context_window: 128000,
            ..Default::default()
        };
        assert_eq!(state.context_usage_pct(), 10);
    }

    #[test]
    fn context_usage_pct_zero_window() {
        let state = StatusLineState::default();
        assert_eq!(state.context_usage_pct(), 0);
    }

    #[test]
    fn context_usage_pct_capped_at_100() {
        let state = StatusLineState {
            total_tokens: 200000,
            context_window: 128000,
            ..Default::default()
        };
        assert_eq!(state.context_usage_pct(), 100);
    }

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(1), "1");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(1_000), "1.0k");
        assert_eq!(format_tokens(12_800), "12.8k");
        assert_eq!(format_tokens(128_000), "128.0k");
        assert_eq!(format_tokens(999_999), "1000.0k");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_500_000), "2.5M");
        assert_eq!(format_tokens(10_000_000), "10.0M");
    }
}
