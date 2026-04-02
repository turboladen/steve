# Clickable Links in Message Area

**Issue:** steve-hshc — Links in messages window not clickable via terminal (cmd-click)

## Problem

URLs in Steve's message area aren't cmd-clickable in terminals that support it (Ghostty,
iTerm2, Kitty). Two causes:

1. **Markdown links** `[text](url)` split into 2 spans — link text (styled) and ` (url)` (dim).
   The URL is wrapped in parentheses, which breaks terminal regex-based URL auto-detection.
2. **Bare URLs** in prose text get no special treatment — they work with terminal auto-detection
   IF they happen to land in a single contiguous span, but this is incidental, not guaranteed,
   and they have no visual link styling.

## Rejected: OSC 8 Escape Sequences

The initial approach was to embed OSC 8 hyperlink escapes (`\x1b]8;;URL\x1b\`) in Span
content. This was rejected because `unicode-width` (used by ratatui's `Paragraph::wrap()` and
our `content_height` calculation) treats ESC (U+001B) as width 1. A typical URL's OSC 8
overhead adds ~35 invisible characters, each miscounted as display width 1, completely breaking
line wrapping and scroll math.

The hyperrat crate solves this by writing to `Buffer` directly (bypassing `Paragraph`), but
that doesn't work for inline links within flowing text.

## Approach: Clean Span Structure for Terminal Auto-Detection

Modern terminals (Ghostty, iTerm2, Kitty) all have regex-based URL auto-detection — they
scan the terminal buffer for URL patterns and make matching text cmd-clickable. This works
as long as the URL appears as **clean, contiguous text** without wrapping characters that
break the regex.

Two changes in `parse_inline_spans()` in `markdown.rs`:

### 1. Markdown links `[text](url)`

**Before:** Two spans:
- `Span::styled(link_text, link_color + UNDERLINED)` — "docs"
- `Span::styled(" (url)", dim)` — " (https://example.com)"

The parentheses break auto-detection in some terminals.

**After:** Three spans:
- `Span::styled(link_text, link_color + UNDERLINED)` — "docs"
- `Span::styled(" ", base_style)` — separator space
- `Span::styled(url, dim + UNDERLINED)` — "https://example.com"

The URL span is clean (no parens), contiguous, and underlined for visual affordance. The
`plain` field includes the URL for correct wrapping width: `"link_text url"`.

### 2. Bare URL detection

Add URL scanning in the main character loop of `parse_inline_spans()`. When building the
plain text buffer, check if it contains `https://` or `http://` at a word boundary. When
found, scan forward to find the URL end, then:

- Flush any text before the URL as a normal span
- Emit the URL as `Span::styled(url, link_color + UNDERLINED)`
- The `plain` field is the URL text itself (matches display width exactly)

**URL boundary detection — conservative approach:**

A URL starts at `https://` or `http://` and ends at:
- Whitespace or end of input
- Trailing punctuation stripped: `.` `,` `;` `:` `!` `?` `>` at the very end
- Closing `)` stripped only if unbalanced (preserves Wikipedia-style URLs like
  `https://en.wikipedia.org/wiki/Rust_(programming_language)`)

**Not matched:**
- `www.` without scheme prefix
- Email addresses
- Strings like "httpbin" or "https" without `://`

## What does NOT change

- `MarkdownLine.raw` — remains original markdown source, used for clipboard copy
- Rendering pipeline (`GutteredLines`, `Paragraph`, `ContentMap`) — no structural changes
- No new dependencies
- Width math — all spans contain only visible text (no escape sequences)

## Testing

Unit tests in `markdown.rs`:

**Markdown links:**
- Link renders with clean URL span (no parens)
- `plain` field includes both link text and URL
- `raw` field preserves original markdown syntax

**Bare URL detection:**
- `https://example.com` detected and styled
- `http://example.com` detected and styled
- URL at end of line detected
- URL followed by `.` or `,` — punctuation stripped from URL
- URL with balanced parens preserved: `https://en.wikipedia.org/wiki/Foo_(bar)`
- URL with trailing `)` stripped when unbalanced
- `httpbin` not falsely matched (no `://`)
- `https` alone not matched
- URL mid-sentence: "visit https://example.com for details"
- Multiple URLs in one line both detected

**Manual testing:** cmd-click in Ghostty on rendered URLs

## Future: OSC 8 (follow-up)

If terminal auto-detection proves insufficient, a post-render buffer pass could inject
OSC 8 sequences directly into `frame.buffer_mut()` cells at known link positions. This
would require mapping logical link positions through wrapping + scroll offset to buffer
coordinates. Tracked separately if needed.
