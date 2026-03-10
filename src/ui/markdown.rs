use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::theme::Theme;

/// A rendered markdown line with both styled spans (for ratatui) and plain text
/// (for ContentMap / clipboard selection, with markdown syntax stripped).
pub struct MarkdownLine<'a> {
    pub styled: Line<'a>,
    pub plain: String,
}

/// Render a single line of markdown-formatted text into styled spans.
///
/// Handles line-level prefixes (headers, bullets, horizontal rules) and
/// inline formatting (bold, italic, code, links). Code blocks are handled
/// at a higher level in `render_text_with_code_blocks` — this function
/// only sees prose lines.
pub fn render_markdown_line(line: &str, theme: &Theme, available_width: usize) -> MarkdownLine<'static> {
    // Horizontal rule: 3+ of the same char (-, *, _) optionally with spaces
    if is_horizontal_rule(line) {
        let rule = "\u{2500}".repeat(available_width);
        return MarkdownLine {
            styled: Line::from(Span::styled(
                rule,
                Style::default().fg(theme.dim),
            )),
            plain: String::new(),
        };
    }

    // Header: ^#{1,6}\s+(.*)$
    if let Some((level, content)) = parse_header(line) {
        let inline = parse_inline_spans(content, theme);
        let plain = inline.iter().map(|s| s.plain.as_str()).collect::<String>();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let style = Style::default()
            .fg(theme.heading)
            .add_modifier(Modifier::BOLD);
        // Add hash prefix for visual hierarchy indication
        let prefix = "#".repeat(level) + " ";
        spans.push(Span::styled(prefix, style));
        for s in inline {
            // Heading color + bold are mandatory; keep any extra modifiers from inline parsing
            let merged = s.span.style.patch(style);
            spans.push(Span::styled(s.span.content.into_owned(), merged));
        }
        return MarkdownLine {
            styled: Line::from(spans),
            plain,
        };
    }

    // Bullet list: ^(\s*)([-*])\s+(.*)$ or ^(\s*)(\d+\.)\s+(.*)$
    if let Some((indent, marker, content)) = parse_list_item(line) {
        let inline = parse_inline_spans(content, theme);
        let plain_content: String = inline.iter().map(|s| s.plain.as_str()).collect();
        let plain = format!("{indent}{marker} {plain_content}");
        let mut spans: Vec<Span<'static>> = Vec::new();
        let base_style = Style::default().fg(theme.assistant_msg);
        spans.push(Span::styled(format!("{indent}{marker} "), base_style));
        for s in inline {
            spans.push(s.span);
        }
        return MarkdownLine {
            styled: Line::from(spans),
            plain,
        };
    }

    // Normal prose — just inline parsing
    let inline = parse_inline_spans(line, theme);
    let plain: String = inline.iter().map(|s| s.plain.as_str()).collect();
    let spans: Vec<Span<'static>> = inline.into_iter().map(|s| s.span).collect();
    MarkdownLine {
        styled: Line::from(spans),
        plain,
    }
}

/// A parsed inline span with its styled representation and plain text.
struct InlineSpan<'a> {
    span: Span<'a>,
    plain: String,
}

/// Parse inline markdown formatting from text, returning styled spans.
///
/// Priority order: backtick code > bold+italic > bold > italic > links > plain.
/// Unmatched delimiters are emitted as plain text for graceful streaming.
fn parse_inline_spans(text: &str, theme: &Theme) -> Vec<InlineSpan<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut plain_buf = String::new();
    let base_style = Style::default().fg(theme.assistant_msg);

    while i < len {
        let ch = chars[i];

        // Backtick: inline code (highest priority — nothing parsed inside)
        if ch == '`' {
            // Flush plain buffer
            if !plain_buf.is_empty() {
                spans.push(InlineSpan {
                    span: Span::styled(plain_buf.clone(), base_style),
                    plain: plain_buf.clone(),
                });
                plain_buf.clear();
            }

            if let Some((code_text, end)) = scan_inline_code(&chars, i) {
                spans.push(InlineSpan {
                    span: Span::styled(
                        code_text.clone(),
                        Style::default().fg(theme.assistant_msg).bg(theme.inline_code_bg),
                    ),
                    plain: code_text,
                });
                i = end;
                continue;
            }
            // Unmatched backtick — emit as plain
            plain_buf.push('`');
            i += 1;
            continue;
        }

        // Link: [text](url)
        if ch == '[' {
            if let Some((link_text, url, end)) = scan_link(&chars, i) {
                if !plain_buf.is_empty() {
                    spans.push(InlineSpan {
                        span: Span::styled(plain_buf.clone(), base_style),
                        plain: plain_buf.clone(),
                    });
                    plain_buf.clear();
                }
                spans.push(InlineSpan {
                    span: Span::styled(
                        link_text.clone(),
                        Style::default().fg(theme.link).add_modifier(Modifier::UNDERLINED),
                    ),
                    plain: link_text,
                });
                spans.push(InlineSpan {
                    span: Span::styled(
                        format!(" ({url})"),
                        Style::default().fg(theme.dim),
                    ),
                    plain: String::new(), // URL not in plain text
                });
                i = end;
                continue;
            }
            // Not a valid link — emit [ as plain
            plain_buf.push('[');
            i += 1;
            continue;
        }

        // Asterisk emphasis: ***, **, *
        if ch == '*' {
            if let Some((content, modifier, end)) = scan_emphasis(&chars, i) {
                if !plain_buf.is_empty() {
                    spans.push(InlineSpan {
                        span: Span::styled(plain_buf.clone(), base_style),
                        plain: plain_buf.clone(),
                    });
                    plain_buf.clear();
                }
                // Recursively parse inline spans within the emphasis content
                let inner = parse_inline_spans(&content, theme);
                for mut s in inner {
                    s.span.style = s.span.style.add_modifier(modifier);
                    spans.push(s);
                }
                i = end;
                continue;
            }
            // Unmatched * — emit as plain
            plain_buf.push('*');
            i += 1;
            continue;
        }

        // Regular character
        plain_buf.push(ch);
        i += 1;
    }

    // Flush remaining plain text
    if !plain_buf.is_empty() {
        spans.push(InlineSpan {
            span: Span::styled(plain_buf.clone(), base_style),
            plain: plain_buf,
        });
    }

    spans
}

/// Scan for inline code starting at position `i` (which is a backtick).
/// Returns (code_content, end_position) or None.
fn scan_inline_code(chars: &[char], start: usize) -> Option<(String, usize)> {
    let len = chars.len();
    // Count opening backticks
    let mut ticks = 0;
    let mut i = start;
    while i < len && chars[i] == '`' {
        ticks += 1;
        i += 1;
    }

    // Find matching closing backticks
    let mut content = String::new();
    while i < len {
        // Check for closing sequence
        let mut closing = 0;
        while i < len && chars[i] == '`' {
            closing += 1;
            i += 1;
        }
        if closing == ticks {
            // Trim single leading/trailing space per CommonMark spec
            let trimmed = if content.starts_with(' ') && content.ends_with(' ') && content.len() > 1 {
                content[1..content.len() - 1].to_string()
            } else {
                content
            };
            return Some((trimmed, i));
        }
        if closing > 0 {
            // Not the right number of backticks — add them as content
            for _ in 0..closing {
                content.push('`');
            }
        } else {
            content.push(chars[i]);
            i += 1;
        }
    }
    None // No matching close
}

/// Scan for a markdown link `[text](url)` starting at position `i` (which is `[`).
/// Returns (link_text, url, end_position) or None.
fn scan_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let len = chars.len();
    let mut i = start + 1; // skip [
    let mut text = String::new();
    let mut depth = 1;

    // Find closing ]
    while i < len && depth > 0 {
        if chars[i] == '[' {
            depth += 1;
        } else if chars[i] == ']' {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
        text.push(chars[i]);
        i += 1;
    }
    if depth != 0 || i >= len {
        return None;
    }
    i += 1; // skip ]

    // Must immediately follow with (
    if i >= len || chars[i] != '(' {
        return None;
    }
    i += 1; // skip (

    let mut url = String::new();
    let mut paren_depth = 1;
    while i < len && paren_depth > 0 {
        if chars[i] == '(' {
            paren_depth += 1;
        } else if chars[i] == ')' {
            paren_depth -= 1;
            if paren_depth == 0 {
                break;
            }
        }
        url.push(chars[i]);
        i += 1;
    }
    if paren_depth != 0 {
        return None;
    }
    i += 1; // skip )

    Some((text, url, i))
}

/// Scan for emphasis (*text*, **text**, ***text***) starting at position `i`.
/// Returns (content, Modifier, end_position) or None.
fn scan_emphasis(chars: &[char], start: usize) -> Option<(String, Modifier, usize)> {
    let len = chars.len();
    // Count opening asterisks (1-3)
    let mut stars = 0;
    let mut i = start;
    while i < len && chars[i] == '*' && stars < 3 {
        stars += 1;
        i += 1;
    }

    if stars == 0 || i >= len {
        return None;
    }

    // Content must not start with a space
    if chars[i] == ' ' {
        return None;
    }

    // Find the matching closing sequence
    let mut content = String::new();
    while i < len {
        // Check for closing asterisks
        if chars[i] == '*' {
            let mut closing = 0;
            let j = i;
            while i < len && chars[i] == '*' && closing < stars {
                closing += 1;
                i += 1;
            }
            if closing == stars {
                // Content must not end with a space
                if content.ends_with(' ') || content.is_empty() {
                    // Put the asterisks back as content and continue
                    for _ in 0..closing {
                        content.push('*');
                    }
                    continue;
                }
                let modifier = if stars == 3 {
                    Modifier::BOLD | Modifier::ITALIC
                } else if stars == 2 {
                    Modifier::BOLD
                } else {
                    Modifier::ITALIC
                };
                return Some((content, modifier, i));
            }
            // Not enough closing stars — add them as content
            for k in j..i {
                content.push(chars[k]);
            }
        } else {
            content.push(chars[i]);
            i += 1;
        }
    }
    None // No matching close
}

/// Check if a line is a horizontal rule (3+ of same char: -, *, _ with optional spaces).
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let no_spaces: String = trimmed.chars().filter(|c| *c != ' ').collect();
    if no_spaces.len() < 3 {
        return false;
    }
    let first = no_spaces.chars().next().unwrap();
    matches!(first, '-' | '*' | '_') && no_spaces.chars().all(|c| c == first)
}

/// Parse a markdown header line. Returns (level, content) or None.
fn parse_header(line: &str) -> Option<(usize, &str)> {
    let mut level = 0;
    let bytes = line.as_bytes();
    while level < bytes.len() && level < 6 && bytes[level] == b'#' {
        level += 1;
    }
    if level == 0 {
        return None;
    }
    // Must be followed by a space
    if level >= bytes.len() || bytes[level] != b' ' {
        return None;
    }
    Some((level, &line[level + 1..]))
}

/// Parse a list item. Returns (indent, marker, content) or None.
/// Unordered: `  - item` or `  * item`
/// Ordered: `  1. item`
fn parse_list_item(line: &str) -> Option<(&str, String, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;

    // Count leading whitespace
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let indent = &line[..i];

    if i >= bytes.len() {
        return None;
    }

    // Unordered: - or * followed by space
    if (bytes[i] == b'-' || bytes[i] == b'*') && i + 1 < bytes.len() && bytes[i + 1] == b' ' {
        // Don't match horizontal rules (--- or ***)
        let rest_of_marker: &[u8] = &bytes[i..];
        if is_horizontal_rule(&line[i..]) {
            return None;
        }
        let _ = rest_of_marker;
        let marker = "\u{2022}".to_string(); // bullet •
        let content = &line[i + 2..];
        return Some((indent, marker, content));
    }

    // Ordered: digits followed by . and space
    let digit_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > digit_start && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
        let number = &line[digit_start..i];
        let marker = format!("{number}.");
        let content = &line[i + 2..];
        return Some((indent, marker, content));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Theme {
        Theme::dark()
    }

    // -- Inline code --

    #[test]
    fn inline_code_basic() {
        let result = render_markdown_line("use `foo` here", &dark(), 40);
        assert_eq!(result.plain, "use foo here");
        assert!(result.styled.spans.len() >= 3, "expected at least 3 spans: text, code, text");
        // The code span should have inline_code_bg
        let code_span = &result.styled.spans[1];
        assert_eq!(code_span.style.bg, Some(dark().inline_code_bg));
        assert_eq!(code_span.content.as_ref(), "foo");
    }

    #[test]
    fn inline_code_double_backtick() {
        let result = render_markdown_line("use ``foo `bar` baz`` here", &dark(), 40);
        assert_eq!(result.plain, "use foo `bar` baz here");
    }

    #[test]
    fn inline_code_unmatched() {
        let result = render_markdown_line("use `foo here", &dark(), 40);
        assert_eq!(result.plain, "use `foo here");
    }

    // -- Bold --

    #[test]
    fn bold_basic() {
        let result = render_markdown_line("this is **bold** text", &dark(), 40);
        assert_eq!(result.plain, "this is bold text");
        // Find the bold span
        let bold_span = result.styled.spans.iter().find(|s| s.content.as_ref() == "bold").unwrap();
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bold_unmatched() {
        let result = render_markdown_line("this is **bold text", &dark(), 40);
        assert_eq!(result.plain, "this is **bold text");
    }

    // -- Italic --

    #[test]
    fn italic_basic() {
        let result = render_markdown_line("this is *italic* text", &dark(), 40);
        assert_eq!(result.plain, "this is italic text");
        let italic_span = result.styled.spans.iter().find(|s| s.content.as_ref() == "italic").unwrap();
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    // -- Bold + Italic --

    #[test]
    fn bold_italic() {
        let result = render_markdown_line("this is ***important*** text", &dark(), 40);
        assert_eq!(result.plain, "this is important text");
        let span = result.styled.spans.iter().find(|s| s.content.as_ref() == "important").unwrap();
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }

    // -- Combined inline elements --

    #[test]
    fn combined_bold_and_italic() {
        let result = render_markdown_line("**bold** and *italic*", &dark(), 40);
        assert_eq!(result.plain, "bold and italic");
    }

    #[test]
    fn combined_bold_and_code() {
        let result = render_markdown_line("**bold** and `code`", &dark(), 40);
        assert_eq!(result.plain, "bold and code");
    }

    // -- Links --

    #[test]
    fn link_basic() {
        let result = render_markdown_line("see [docs](https://example.com) here", &dark(), 40);
        assert_eq!(result.plain, "see docs here");
        let link_span = result.styled.spans.iter().find(|s| s.content.as_ref() == "docs").unwrap();
        assert_eq!(link_span.style.fg, Some(dark().link));
        assert!(link_span.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn link_unmatched_bracket() {
        let result = render_markdown_line("use [foo here", &dark(), 40);
        assert_eq!(result.plain, "use [foo here");
    }

    // -- Headers --

    #[test]
    fn header_h1() {
        let result = render_markdown_line("# Hello World", &dark(), 40);
        assert_eq!(result.plain, "Hello World");
        // All spans should have heading color
        for span in &result.styled.spans {
            assert_eq!(span.style.fg, Some(dark().heading));
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn header_h3() {
        let result = render_markdown_line("### Sub heading", &dark(), 40);
        assert_eq!(result.plain, "Sub heading");
    }

    #[test]
    fn header_h6() {
        let result = render_markdown_line("###### Deep", &dark(), 40);
        assert_eq!(result.plain, "Deep");
    }

    #[test]
    fn header_no_space_not_header() {
        let result = render_markdown_line("#notaheader", &dark(), 40);
        assert_eq!(result.plain, "#notaheader");
    }

    #[test]
    fn header_with_inline_formatting() {
        let result = render_markdown_line("## The **bold** header", &dark(), 40);
        assert_eq!(result.plain, "The bold header");
    }

    // -- Bullet lists --

    #[test]
    fn unordered_list_dash() {
        let result = render_markdown_line("- item one", &dark(), 40);
        assert_eq!(result.plain, "\u{2022} item one");
    }

    #[test]
    fn unordered_list_star() {
        let result = render_markdown_line("* item two", &dark(), 40);
        assert_eq!(result.plain, "\u{2022} item two");
    }

    #[test]
    fn ordered_list() {
        let result = render_markdown_line("1. first item", &dark(), 40);
        assert_eq!(result.plain, "1. first item");
    }

    #[test]
    fn indented_list() {
        let result = render_markdown_line("  - nested", &dark(), 40);
        assert_eq!(result.plain, "  \u{2022} nested");
    }

    #[test]
    fn list_with_inline_formatting() {
        let result = render_markdown_line("- **bold** item", &dark(), 40);
        assert_eq!(result.plain, "\u{2022} bold item");
    }

    // -- Horizontal rules --

    #[test]
    fn horizontal_rule_dashes() {
        let result = render_markdown_line("---", &dark(), 20);
        assert!(result.plain.is_empty());
        let text: String = result.styled.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('\u{2500}'), "should contain ─ chars");
    }

    #[test]
    fn horizontal_rule_stars() {
        let result = render_markdown_line("***", &dark(), 20);
        assert!(result.plain.is_empty());
    }

    #[test]
    fn horizontal_rule_with_spaces() {
        let result = render_markdown_line("- - -", &dark(), 20);
        assert!(result.plain.is_empty());
    }

    // -- Edge cases --

    #[test]
    fn empty_line() {
        let result = render_markdown_line("", &dark(), 40);
        assert_eq!(result.plain, "");
    }

    #[test]
    fn plain_text_passthrough() {
        let result = render_markdown_line("just normal text", &dark(), 40);
        assert_eq!(result.plain, "just normal text");
        assert_eq!(result.styled.spans.len(), 1);
    }

    #[test]
    fn empty_bold_not_parsed() {
        let result = render_markdown_line("**** empty", &dark(), 40);
        // **** has no content between — not valid emphasis
        assert_eq!(result.plain, "**** empty");
    }

    #[test]
    fn asterisk_in_math_not_emphasis() {
        // Single * surrounded by spaces should not trigger emphasis
        let result = render_markdown_line("2 * 3 = 6", &dark(), 40);
        assert_eq!(result.plain, "2 * 3 = 6");
    }

    #[test]
    fn three_dashes_not_list_item() {
        // "---" should be a horizontal rule, not a list item
        let result = render_markdown_line("---", &dark(), 20);
        assert!(result.plain.is_empty(), "--- should be a horizontal rule");
    }
}
