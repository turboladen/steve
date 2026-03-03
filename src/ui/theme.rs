use ratatui::style::Color;

/// Color palette for the TUI — "Warm Terminal" identity.
/// Amber/gold accent, warm grays, coral for write operations.
#[derive(Debug, PartialEq)]
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
    pub tool_read: Color,
    pub tool_write: Color,
    pub reasoning: Color,
    pub border: Color,
    pub mode_build: Color,
    pub mode_plan: Color,
    pub permission: Color,
    pub code_bg: Color,
    pub context_amber: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// Return the border color shifted by context window pressure.
    ///
    /// | Range   | Color                     | Visual           |
    /// |---------|---------------------------|------------------|
    /// | <40%    | `self.border`             | Normal warm gray |
    /// | 40–59%  | `self.context_amber`      | Warm amber-brown |
    /// | 60–79%  | `self.warning`            | Yellow           |
    /// | 80%+    | `self.error`              | Red              |
    pub fn border_color(&self, context_pct: u8) -> Color {
        if context_pct >= 80 {
            self.error
        } else if context_pct >= 60 {
            self.warning
        } else if context_pct >= 40 {
            self.context_amber
        } else {
            self.border
        }
    }

    /// Default dark theme with warm palette.
    pub fn dark() -> Self {
        Self {
            fg: Color::White,
            bg: Color::Reset,
            accent: Color::Rgb(255, 170, 51),       // Amber/Gold
            dim: Color::Rgb(100, 95, 85),            // Warm gray
            error: Color::Rgb(255, 85, 85),          // Warm red
            warning: Color::Rgb(255, 200, 60),       // Warm yellow
            success: Color::Rgb(80, 200, 120),       // Warm green
            user_msg: Color::Rgb(230, 225, 215),     // Soft warm white
            assistant_msg: Color::Rgb(220, 218, 210), // Warm off-white
            tool_read: Color::Rgb(120, 115, 105),    // Muted warm gray
            tool_write: Color::Rgb(255, 120, 80),    // Coral/Orange
            reasoning: Color::Rgb(150, 140, 170),    // Muted lavender
            border: Color::Rgb(88, 88, 88),          // Warm gray
            mode_build: Color::Rgb(255, 170, 51),    // Amber
            mode_plan: Color::Rgb(100, 149, 237),    // Cornflower blue
            permission: Color::Rgb(255, 200, 60),    // Warm yellow
            code_bg: Color::Rgb(35, 33, 30),            // Warm dark tint for code blocks
            context_amber: Color::Rgb(140, 120, 60),    // Amber-brown for 40-59% context pressure
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_equals_dark() {
        assert_eq!(Theme::default(), Theme::dark());
    }

    #[test]
    fn accent_is_amber_rgb() {
        let t = Theme::dark();
        assert!(matches!(t.accent, Color::Rgb(255, 170, 51)));
    }

    #[test]
    fn tool_read_differs_from_tool_write() {
        let t = Theme::dark();
        assert_ne!(t.tool_read, t.tool_write);
    }

    #[test]
    fn reasoning_differs_from_tool_read() {
        let t = Theme::dark();
        assert_ne!(t.reasoning, t.tool_read);
    }

    #[test]
    fn code_bg_is_rgb() {
        let t = Theme::dark();
        assert!(matches!(t.code_bg, Color::Rgb(35, 33, 30)));
    }

    #[test]
    fn code_bg_differs_from_bg() {
        let t = Theme::dark();
        assert_ne!(t.code_bg, t.bg);
    }

    #[test]
    fn border_color_below_40_returns_border() {
        let t = Theme::dark();
        assert_eq!(t.border_color(0), t.border);
        assert_eq!(t.border_color(39), t.border);
    }

    #[test]
    fn border_color_40_to_59_returns_amber_brown() {
        let t = Theme::dark();
        assert_eq!(t.border_color(40), t.context_amber);
        assert_eq!(t.border_color(59), t.context_amber);
    }

    #[test]
    fn border_color_60_to_79_returns_warning() {
        let t = Theme::dark();
        assert_eq!(t.border_color(60), t.warning);
        assert_eq!(t.border_color(79), t.warning);
    }

    #[test]
    fn border_color_80_plus_returns_error() {
        let t = Theme::dark();
        assert_eq!(t.border_color(80), t.error);
        assert_eq!(t.border_color(100), t.error);
    }

    #[test]
    fn warm_gray_fields_are_rgb() {
        let t = Theme::dark();
        assert!(matches!(t.dim, Color::Rgb(..)));
        assert!(matches!(t.reasoning, Color::Rgb(..)));
        assert!(matches!(t.border, Color::Rgb(..)));
        assert!(matches!(t.tool_read, Color::Rgb(..)));
        assert!(matches!(t.code_bg, Color::Rgb(..)));
    }
}
