use ratatui::style::Color;

/// Color palette for the TUI. Will be made terminal-adaptive in Phase 10.
pub struct Theme {
    pub fg: Color,
    pub bg: Color,
    pub accent: Color,
    pub dim: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    pub user_msg: Color,
    pub assistant_msg: Color,
    pub tool_call: Color,
    pub reasoning: Color,
    pub border: Color,
    pub mode_build: Color,
    pub mode_plan: Color,
    pub permission: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// Default dark theme. Fallback until terminal-adaptive detection is implemented.
    pub fn dark() -> Self {
        Self {
            fg: Color::White,
            bg: Color::Reset,
            accent: Color::Cyan,
            dim: Color::DarkGray,
            error: Color::Red,
            warning: Color::Yellow,
            success: Color::Green,
            user_msg: Color::Blue,
            assistant_msg: Color::White,
            tool_call: Color::Magenta,
            reasoning: Color::DarkGray,
            border: Color::DarkGray,
            mode_build: Color::Green,
            mode_plan: Color::Cyan,
            permission: Color::Yellow,
        }
    }
}
