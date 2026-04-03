use ratatui::style::Color;

/// Context pressure tier thresholds (percentage of context window used).
pub const CONTEXT_TIER_1: u8 = 40;
pub const CONTEXT_TIER_2: u8 = 60;
pub const CONTEXT_TIER_3: u8 = 80;

/// Color palette derived from the Steve brand photo — golden-yellow accent,
/// warm browns, and a colorblind-safe blue-orange status axis.
/// Gold is purely decorative (accent/headings); warnings use orange.
/// Success uses blue-teal (not green) for red/green deficiency safety.
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

    /// Default dark theme — photo-derived, colorblind-safe.
    pub fn dark() -> Self {
        Self {
            fg: Color::Rgb(232, 226, 210), // Warm cream (shirt in golden light)
            bg: Color::Reset,
            accent: Color::Rgb(240, 190, 50), // Golden-yellow (photo background)
            dim: Color::Rgb(120, 105, 80),    // Warm brown (hair/beard tones)
            error: Color::Rgb(235, 100, 55),  // Red-orange (danger ramp top)
            warning: Color::Rgb(230, 145, 45), // Deep orange (not gold — brand-safe)
            success: Color::Rgb(85, 170, 210), // Blue-teal (colorblind-safe, sunglasses)
            user_msg: Color::Rgb(140, 180, 220), // Cool blue ("you" vs warm "Steve")
            user_msg_bg: Color::Rgb(25, 30, 40), // Blue-gray tint
            assistant_msg: Color::Rgb(228, 222, 208), // Warm cream
            tool_read: Color::Rgb(140, 120, 95), // Brown family (brighter than dim)
            tool_write: Color::Rgb(230, 120, 60), // Orange-coral (danger family)
            reasoning: Color::Rgb(185, 160, 120), // Warm sand (skin tones)
            border: Color::Rgb(70, 62, 50),   // Shadow browns
            mode_build: Color::Rgb(240, 190, 50), // Match accent
            mode_plan: Color::Rgb(100, 155, 220), // Cool blue (analysis = cool)
            permission: Color::Rgb(230, 145, 45), // Match warning
            code_bg: Color::Rgb(30, 27, 22),  // Subtle warmth
            context_amber: Color::Rgb(180, 120, 45), // Muted orange (tier 1 pressure)
            system_msg: Color::Rgb(135, 125, 110), // Warm gray-brown
            selection_bg: Color::Rgb(55, 50, 60), // Warm neutral
            heading: Color::Rgb(240, 195, 65), // Near-accent gold
            inline_code_bg: Color::Rgb(48, 42, 34), // Brown-leaning dark tint
            link: Color::Rgb(120, 165, 210),  // Blue (cool family)
            question: Color::Rgb(80, 175, 165), // Teal
        }
    }

    /// Light theme — photo-derived, colorblind-safe, dark text on light backgrounds.
    pub fn light() -> Self {
        Self {
            fg: Color::Rgb(35, 30, 25),                // Dark warm brown
            bg: Color::Reset,                          // Terminal provides background
            accent: Color::Rgb(175, 130, 0),           // Dark gold
            dim: Color::Rgb(130, 115, 90),             // Warm brown mid-tone
            error: Color::Rgb(200, 70, 25),            // Dark red-orange
            warning: Color::Rgb(185, 105, 10),         // Dark orange (not gold)
            success: Color::Rgb(20, 115, 160),         // Dark blue-teal (colorblind-safe)
            user_msg: Color::Rgb(35, 85, 155),         // Dark blue
            user_msg_bg: Color::Rgb(220, 232, 245),    // Subtle light blue tint
            assistant_msg: Color::Rgb(42, 38, 30),     // Near-black, warm
            tool_read: Color::Rgb(110, 100, 80),       // Warm brown
            tool_write: Color::Rgb(185, 80, 20),       // Dark orange-coral
            reasoning: Color::Rgb(130, 105, 70),       // Dark sand (skin tones)
            border: Color::Rgb(195, 185, 170),         // Warm light border
            mode_build: Color::Rgb(175, 130, 0),       // Match accent
            mode_plan: Color::Rgb(30, 90, 160),        // Dark blue
            permission: Color::Rgb(185, 105, 10),      // Match warning
            code_bg: Color::Rgb(242, 236, 225),        // Warm off-white
            context_amber: Color::Rgb(165, 100, 20),   // Dark muted orange
            system_msg: Color::Rgb(100, 90, 75),       // Warm dark gray
            selection_bg: Color::Rgb(210, 200, 170),   // Warm golden selection tint
            heading: Color::Rgb(160, 115, 0),          // Dark gold
            inline_code_bg: Color::Rgb(235, 228, 215), // Warm light tint
            link: Color::Rgb(20, 80, 150),             // Dark blue
            question: Color::Rgb(0, 120, 115),         // Dark teal
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
    fn accent_is_gold_rgb() {
        let t = Theme::dark();
        assert!(matches!(t.accent, Color::Rgb(240, 190, 50)));
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
        assert!(matches!(t.code_bg, Color::Rgb(30, 27, 22)));
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
