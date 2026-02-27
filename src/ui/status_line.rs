//! Status line state and rendering for the TUI footer.

use crate::tool::ToolName;

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
}
