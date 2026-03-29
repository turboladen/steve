use std::sync::LazyLock;

use ratatui::{
    style::{Color, Style},
    text::Span,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{self, Theme, ThemeSet},
    parsing::SyntaxSet,
};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let ts = ThemeSet::load_defaults();
    ts.themes["base16-ocean.dark"].clone()
});

/// Returns a reference to the global syntax set (needed for `highlight_line` calls).
pub fn syntax_set() -> &'static SyntaxSet {
    &SYNTAX_SET
}

/// Try to create a highlighter for the given language label.
/// Returns `None` for empty/unknown languages.
pub fn try_highlighter(lang: &str) -> Option<HighlightLines<'static>> {
    if lang.is_empty() {
        return None;
    }
    let syntax = SYNTAX_SET.find_syntax_by_token(lang)?;
    Some(HighlightLines::new(syntax, &THEME))
}

/// Convert syntect highlighted regions into ratatui spans.
/// Forces `code_bg` as the background on every span for visual consistency.
pub fn syntect_to_spans(
    regions: &[(highlighting::Style, &str)],
    code_bg: Color,
) -> Vec<Span<'static>> {
    regions
        .iter()
        .map(|(style, text)| {
            let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
            Span::styled(text.to_string(), Style::default().fg(fg).bg(code_bg))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_set_loads() {
        // LazyLock initializes without panic and contains syntaxes
        assert!(!syntax_set().syntaxes().is_empty());
    }

    #[test]
    fn try_highlighter_known_lang() {
        for lang in ["rust", "python", "js", "go", "java", "c", "bash"] {
            assert!(
                try_highlighter(lang).is_some(),
                "expected highlighter for '{lang}'"
            );
        }
    }

    #[test]
    fn try_highlighter_unknown_lang() {
        assert!(
            try_highlighter("").is_none(),
            "empty string should return None"
        );
        assert!(
            try_highlighter("nonexistent_gibberish_lang_42").is_none(),
            "unknown language should return None"
        );
    }

    #[test]
    fn syntect_to_spans_preserves_text() {
        let mut h = try_highlighter("rust").unwrap();
        let line = "fn main() {}";
        let regions = h.highlight_line(line, syntax_set()).unwrap();
        let spans = syntect_to_spans(&regions, Color::Rgb(28, 26, 23));
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, line, "concatenated spans should match input");
    }

    #[test]
    fn syntect_to_spans_uses_code_bg() {
        let code_bg = Color::Rgb(28, 26, 23);
        let mut h = try_highlighter("rust").unwrap();
        let regions = h.highlight_line("let x = 42;", syntax_set()).unwrap();
        let spans = syntect_to_spans(&regions, code_bg);
        assert!(!spans.is_empty());
        for span in &spans {
            assert_eq!(
                span.style.bg,
                Some(code_bg),
                "every span should have code_bg background"
            );
        }
    }

    #[test]
    fn syntect_to_spans_produces_distinct_fg_colors() {
        let mut h = try_highlighter("python").unwrap();
        let regions = h.highlight_line("def foo(): pass", syntax_set()).unwrap();
        let spans = syntect_to_spans(&regions, Color::Rgb(28, 26, 23));
        // Syntax highlighting should produce at least 2 distinct foreground colors
        // (e.g., keyword vs identifier) — not just a single color for everything
        let fg_colors: std::collections::HashSet<_> = spans.iter().map(|s| s.style.fg).collect();
        assert!(
            fg_colors.len() > 1,
            "syntax highlighting should produce distinct fg colors, got {:?}",
            fg_colors
        );
    }
}
