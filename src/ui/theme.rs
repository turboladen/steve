use ratatui::style::Color;

/// Context pressure tier thresholds (percentage of context window used).
pub const CONTEXT_TIER_1: u8 = 40;
pub const CONTEXT_TIER_2: u8 = 60;
pub const CONTEXT_TIER_3: u8 = 80;

/// Color palette for the TUI — "Warm Terminal" identity.
/// Rich amber accent, warm grays, coral for write operations.
/// User messages use a distinct blue tint for immediate visual separation.
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
    pub user_msg_bg: Color,
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
    pub system_msg: Color,
    pub selection_bg: Color,
    pub heading: Color,
    pub inline_code_bg: Color,
    pub link: Color,
    pub question: Color,
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
        if context_pct >= CONTEXT_TIER_3 {
            self.error
        } else if context_pct >= CONTEXT_TIER_2 {
            self.warning
        } else if context_pct >= CONTEXT_TIER_1 {
            self.context_amber
        } else {
            self.border
        }
    }

    /// Return the text color for context pressure indicators.
    ///
    /// Similar to `border_color()` but maps the lowest tier to `self.dim`
    /// (for token counters) instead of `self.border` (for borders).
    pub fn context_color(&self, context_pct: u8) -> Color {
        if context_pct >= CONTEXT_TIER_3 {
            self.error
        } else if context_pct >= CONTEXT_TIER_2 {
            self.warning
        } else if context_pct >= CONTEXT_TIER_1 {
            self.context_amber
        } else {
            self.dim
        }
    }

    /// Default dark theme with warm palette.
    pub fn dark() -> Self {
        Self {
            fg: Color::Rgb(230, 228, 222), // Warm cream (softer than pure white)
            bg: Color::Reset,
            accent: Color::Rgb(230, 165, 55), // Rich amber (warmer, refined)
            dim: Color::Rgb(110, 105, 95),    // Warm gray (readable secondary text)
            error: Color::Rgb(235, 90, 90),   // Soft red (less harsh)
            warning: Color::Rgb(240, 190, 60), // Warm gold
            success: Color::Rgb(85, 195, 120), // Soft green
            user_msg: Color::Rgb(145, 185, 225), // Soft blue (distinct from assistant)
            user_msg_bg: Color::Rgb(25, 30, 40), // Barely-visible blue-gray tint
            assistant_msg: Color::Rgb(225, 222, 215), // Warm cream
            tool_read: Color::Rgb(130, 125, 115), // Warm mid-gray (brighter than dim)
            tool_write: Color::Rgb(240, 120, 85), // Warm coral (slightly softer)
            reasoning: Color::Rgb(165, 150, 190), // Richer lavender
            border: Color::Rgb(65, 63, 60),   // Subtle dark border
            mode_build: Color::Rgb(230, 165, 55), // Match accent
            mode_plan: Color::Rgb(110, 155, 230), // Richer blue
            permission: Color::Rgb(240, 190, 60), // Match warning
            code_bg: Color::Rgb(28, 26, 23),  // Darker code background
            context_amber: Color::Rgb(150, 125, 55), // Warm amber-brown
            system_msg: Color::Rgb(130, 145, 160), // Cool slate (distinct from dim)
            selection_bg: Color::Rgb(60, 60, 80), // Subtle blue-gray selection tint
            heading: Color::Rgb(230, 180, 80), // Warm gold for headers
            inline_code_bg: Color::Rgb(45, 42, 38), // Subtle bg tint for inline code
            link: Color::Rgb(130, 170, 210),  // Soft blue for link text
            question: Color::Rgb(90, 185, 180), // Soft teal
        }
    }

    /// Light theme — dark text on light backgrounds.
    /// All RGB values shifted for readability against white/light terminal backgrounds.
    pub fn light() -> Self {
        Self {
            fg: Color::Rgb(30, 30, 30),        // Near-black (dark text on light bg)
            bg: Color::Reset,                  // Terminal provides background
            accent: Color::Rgb(180, 110, 0),   // Dark amber (visible on white)
            dim: Color::Rgb(120, 115, 110),    // Slightly lighter warm gray
            error: Color::Rgb(200, 40, 40),    // Dark red
            warning: Color::Rgb(180, 130, 0),  // Dark gold
            success: Color::Rgb(30, 140, 60),  // Dark green
            user_msg: Color::Rgb(40, 90, 160), // Dark blue
            user_msg_bg: Color::Rgb(220, 230, 245), // Subtle light blue tint
            assistant_msg: Color::Rgb(40, 38, 35), // Near-black
            tool_read: Color::Rgb(100, 95, 90), // Darker gray
            tool_write: Color::Rgb(190, 60, 30), // Dark coral
            reasoning: Color::Rgb(100, 75, 140), // Dark lavender
            border: Color::Rgb(190, 188, 185), // Light gray (subtle on light bg)
            mode_build: Color::Rgb(180, 110, 0), // Match accent
            mode_plan: Color::Rgb(40, 90, 170), // Dark blue
            permission: Color::Rgb(180, 130, 0), // Match warning
            code_bg: Color::Rgb(240, 238, 235), // Off-white (slight tint from bg)
            context_amber: Color::Rgb(160, 120, 40), // Darker amber for light bg
            system_msg: Color::Rgb(70, 90, 110), // Dark slate
            selection_bg: Color::Rgb(180, 210, 240), // Light blue selection tint
            heading: Color::Rgb(160, 100, 0),  // Dark gold for headers
            inline_code_bg: Color::Rgb(230, 228, 225), // Subtle bg tint for inline code
            link: Color::Rgb(30, 80, 150),     // Dark blue for links
            question: Color::Rgb(0, 130, 120), // Dark teal
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
        assert!(matches!(t.accent, Color::Rgb(230, 165, 55)));
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
        assert!(matches!(t.code_bg, Color::Rgb(28, 26, 23)));
    }

    #[test]
    fn code_bg_differs_from_bg() {
        let t = Theme::dark();
        assert_ne!(t.code_bg, t.bg);
    }

    #[test]
    fn inline_code_bg_differs_from_code_bg() {
        let t = Theme::dark();
        assert_ne!(t.inline_code_bg, t.code_bg);
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
        assert!(matches!(t.system_msg, Color::Rgb(..)));
        assert!(matches!(t.heading, Color::Rgb(..)));
        assert!(matches!(t.inline_code_bg, Color::Rgb(..)));
        assert!(matches!(t.link, Color::Rgb(..)));
        assert!(matches!(t.user_msg_bg, Color::Rgb(..)));
    }

    #[test]
    fn user_msg_bg_differs_from_bg() {
        let t = Theme::dark();
        assert_ne!(t.user_msg_bg, t.bg, "user_msg_bg should differ from bg");
    }

    #[test]
    fn context_color_below_40_returns_dim() {
        let t = Theme::dark();
        assert_eq!(t.context_color(0), t.dim);
        assert_eq!(t.context_color(39), t.dim);
    }

    #[test]
    fn context_color_40_to_59_returns_amber() {
        let t = Theme::dark();
        assert_eq!(t.context_color(40), t.context_amber);
        assert_eq!(t.context_color(59), t.context_amber);
    }

    #[test]
    fn context_color_60_to_79_returns_warning() {
        let t = Theme::dark();
        assert_eq!(t.context_color(60), t.warning);
        assert_eq!(t.context_color(79), t.warning);
    }

    #[test]
    fn context_color_80_plus_returns_error() {
        let t = Theme::dark();
        assert_eq!(t.context_color(80), t.error);
        assert_eq!(t.context_color(100), t.error);
    }

    #[test]
    fn question_differs_from_permission() {
        let t = Theme::dark();
        assert_ne!(t.question, t.permission);
    }

    #[test]
    fn question_is_rgb() {
        let t = Theme::dark();
        assert!(matches!(t.question, Color::Rgb(..)));
    }

    #[test]
    fn user_msg_is_blue_tint() {
        let t = Theme::dark();
        // User messages should be a distinct blue, not warm white
        match t.user_msg {
            Color::Rgb(r, _g, b) => assert!(b > r, "user_msg should have blue > red for blue tint"),
            _ => panic!("user_msg should be Rgb"),
        }
    }

    #[test]
    fn system_msg_differs_from_dim() {
        let t = Theme::dark();
        assert_ne!(
            t.system_msg, t.dim,
            "system_msg should be distinct from dim"
        );
    }

    // -- Light theme tests --

    #[test]
    fn light_fg_is_dark() {
        let t = Theme::light();
        match t.fg {
            Color::Rgb(r, g, b) => {
                assert!(r < 80, "light fg red channel should be dark");
                assert!(g < 80, "light fg green channel should be dark");
                assert!(b < 80, "light fg blue channel should be dark");
            }
            _ => panic!("light fg should be Rgb"),
        }
    }

    #[test]
    fn light_bg_is_reset() {
        let t = Theme::light();
        assert_eq!(
            t.bg,
            Color::Reset,
            "light bg should be Reset (terminal provides it)"
        );
    }

    #[test]
    fn light_differs_from_dark_on_fg() {
        assert_ne!(Theme::light().fg, Theme::dark().fg);
    }

    #[test]
    fn light_border_color_pressure_thresholds() {
        let t = Theme::light();
        assert_eq!(t.border_color(0), t.border);
        assert_eq!(t.border_color(39), t.border);
        assert_eq!(t.border_color(40), t.context_amber);
        assert_eq!(t.border_color(59), t.context_amber);
        assert_eq!(t.border_color(60), t.warning);
        assert_eq!(t.border_color(79), t.warning);
        assert_eq!(t.border_color(80), t.error);
        assert_eq!(t.border_color(100), t.error);
    }
}
