# Clickable Links Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make URLs in the message area cmd-clickable by ensuring they appear as clean, contiguous spans that terminals can auto-detect.

**Architecture:** Two changes in `parse_inline_spans()`: (1) markdown links `[text](url)` emit the URL as a clean span without parentheses, (2) bare URLs (`https://`, `http://`) are detected and emitted as styled link spans. No new dependencies, no rendering pipeline changes.

**Tech Stack:** Rust, ratatui 0.30 (Span/Style/Modifier)

**Spec:** `docs/superpowers/specs/2026-04-01-osc8-hyperlinks-design.md`

---

### Task 1: Add bare URL scanner function

**Files:**
- Modify: `src/ui/markdown.rs` (add `scan_bare_url` function near other `scan_*` functions, ~line 376)
- Test: `src/ui/markdown.rs` (`#[cfg(test)] mod tests` block)

- [ ] **Step 1: Write failing tests for `scan_bare_url`**

Add these tests at the end of the `mod tests` block in `src/ui/markdown.rs`:

```rust
// -- Bare URL detection --

#[test]
fn scan_bare_url_https() {
    let chars: Vec<char> = "https://example.com rest".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}

#[test]
fn scan_bare_url_http() {
    let chars: Vec<char> = "http://example.com rest".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("http://example.com".to_string(), 18)));
}

#[test]
fn scan_bare_url_at_end() {
    let chars: Vec<char> = "https://example.com".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}

#[test]
fn scan_bare_url_trailing_period() {
    let chars: Vec<char> = "https://example.com.".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}

#[test]
fn scan_bare_url_trailing_comma() {
    let chars: Vec<char> = "https://example.com,".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}

#[test]
fn scan_bare_url_trailing_exclamation() {
    let chars: Vec<char> = "https://example.com!".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}

#[test]
fn scan_bare_url_balanced_parens() {
    let chars: Vec<char> = "https://en.wikipedia.org/wiki/Rust_(programming_language) rest"
        .chars()
        .collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(
        result,
        Some((
            "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string(),
            57
        ))
    );
}

#[test]
fn scan_bare_url_unbalanced_trailing_paren() {
    let chars: Vec<char> = "https://example.com)".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}

#[test]
fn scan_bare_url_not_a_url() {
    let chars: Vec<char> = "httpbin is a tool".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, None);
}

#[test]
fn scan_bare_url_https_alone() {
    let chars: Vec<char> = "https alone".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, None);
}

#[test]
fn scan_bare_url_with_path_and_query() {
    let chars: Vec<char> = "https://example.com/path?q=1&r=2#frag rest".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(
        result,
        Some(("https://example.com/path?q=1&r=2#frag".to_string(), 37))
    );
}

#[test]
fn scan_bare_url_mid_string() {
    let chars: Vec<char> = "xxhttps://example.com rest".chars().collect();
    // Starting at position 2 (the 'h' of https)
    let result = scan_bare_url(&chars, 2);
    assert_eq!(result, Some(("https://example.com".to_string(), 21)));
}

#[test]
fn scan_bare_url_trailing_semicolon() {
    let chars: Vec<char> = "https://example.com;".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}

#[test]
fn scan_bare_url_trailing_angle_bracket() {
    let chars: Vec<char> = "https://example.com>".chars().collect();
    let result = scan_bare_url(&chars, 0);
    assert_eq!(result, Some(("https://example.com".to_string(), 19)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ui::markdown::tests::scan_bare_url 2>&1 | tail -20`
Expected: compilation error — `scan_bare_url` not defined

- [ ] **Step 3: Implement `scan_bare_url`**

Add this function in `src/ui/markdown.rs` after the `scan_emphasis` function (after line 376):

```rust
/// Scan for a bare URL starting at position `start` in the char slice.
/// Matches `https://` or `http://` followed by non-whitespace characters.
/// Strips trailing punctuation that's unlikely to be part of the URL.
/// Returns (url_string, end_position) or None.
fn scan_bare_url(chars: &[char], start: usize) -> Option<(String, usize)> {
    let len = chars.len();
    if start >= len {
        return None;
    }

    // Check for http:// or https:// prefix
    let remaining: String = chars[start..].iter().collect();
    let scheme_len = if remaining.starts_with("https://") {
        8
    } else if remaining.starts_with("http://") {
        7
    } else {
        return None;
    };

    // Must have at least one char after the scheme
    if start + scheme_len >= len {
        return None;
    }

    // Scan forward until whitespace or end
    let mut end = start + scheme_len;
    while end < len && !chars[end].is_whitespace() {
        end += 1;
    }

    // Strip trailing punctuation that's unlikely to be part of the URL
    let trailing_punct = ['.', ',', ';', ':', '!', '?', '>'];
    while end > start + scheme_len {
        let last = chars[end - 1];
        if trailing_punct.contains(&last) {
            end -= 1;
        } else if last == ')' {
            // Only strip ) if parens are unbalanced in the URL
            let url_chars = &chars[start..end];
            let open = url_chars.iter().filter(|&&c| c == '(').count();
            let close = url_chars.iter().filter(|&&c| c == ')').count();
            if close > open {
                end -= 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    // Must have content after the scheme
    if end <= start + scheme_len {
        return None;
    }

    let url: String = chars[start..end].iter().collect();
    Some((url, end))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ui::markdown::tests::scan_bare_url 2>&1 | tail -20`
Expected: all `scan_bare_url_*` tests PASS

- [ ] **Step 5: Run clippy**

Run: `cargo clippy 2>&1 | tail -10`
Expected: no new warnings

- [ ] **Step 6: Commit**

```bash
git add src/ui/markdown.rs
git commit -m "feat: add bare URL scanner for markdown inline parsing"
```

---

### Task 2: Integrate bare URL detection into `parse_inline_spans`

**Files:**
- Modify: `src/ui/markdown.rs` — `parse_inline_spans()` function (~line 104) and test block

- [ ] **Step 1: Write failing tests for bare URL rendering**

Add these tests in `src/ui/markdown.rs` after the existing link tests (~line 739):

```rust
// -- Bare URL detection --

#[test]
fn bare_url_https_detected() {
    let result = render_markdown_line("visit https://example.com for info", &dark(), 60);
    assert_eq!(result.plain, "visit https://example.com for info");
    let url_span = result
        .styled
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "https://example.com")
        .expect("should have a URL span");
    assert_eq!(url_span.style.fg, Some(dark().link));
    assert!(url_span.style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn bare_url_http_detected() {
    let result = render_markdown_line("see http://example.com here", &dark(), 60);
    let url_span = result
        .styled
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "http://example.com")
        .expect("should have a URL span");
    assert_eq!(url_span.style.fg, Some(dark().link));
}

#[test]
fn bare_url_not_false_positive() {
    let result = render_markdown_line("use httpbin for testing", &dark(), 60);
    assert_eq!(result.plain, "use httpbin for testing");
    // Should be a single plain span, no link styling
    assert!(
        result
            .styled
            .spans
            .iter()
            .all(|s| s.style.fg != Some(dark().link)),
        "httpbin should not be styled as a link"
    );
}

#[test]
fn bare_url_trailing_period_stripped() {
    let result = render_markdown_line("Go to https://example.com.", &dark(), 60);
    assert_eq!(result.plain, "Go to https://example.com.");
    let url_span = result
        .styled
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "https://example.com")
        .expect("URL span without trailing period");
    assert_eq!(url_span.style.fg, Some(dark().link));
    // The period should be in a separate plain span
    let period_span = result.styled.spans.last().unwrap();
    assert_eq!(period_span.content.as_ref(), ".");
}

#[test]
fn bare_url_multiple_urls() {
    let result = render_markdown_line(
        "see https://a.com and https://b.com",
        &dark(),
        60,
    );
    let url_spans: Vec<_> = result
        .styled
        .spans
        .iter()
        .filter(|s| s.style.fg == Some(dark().link))
        .collect();
    assert_eq!(url_spans.len(), 2);
    assert_eq!(url_spans[0].content.as_ref(), "https://a.com");
    assert_eq!(url_spans[1].content.as_ref(), "https://b.com");
}

#[test]
fn bare_url_preserves_raw() {
    let result = render_markdown_line("visit https://example.com here", &dark(), 60);
    assert_eq!(result.raw, "visit https://example.com here");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ui::markdown::tests::bare_url 2>&1 | tail -25`
Expected: FAIL — URLs not detected (rendered as plain text, no link styling)

- [ ] **Step 3: Add bare URL detection to `parse_inline_spans`**

In `src/ui/markdown.rs`, modify the `parse_inline_spans()` function. Add URL detection at the start of the `while i < len` loop, **before** the backtick check (URLs inside backticks are handled by the backtick branch taking priority). Insert this block right after `let ch = chars[i];` (after line 113):

```rust
        // Bare URL: https:// or http://
        if ch == 'h' {
            if let Some((url, end)) = scan_bare_url(&chars, i) {
                // Flush plain buffer
                if !plain_buf.is_empty() {
                    spans.push(InlineSpan {
                        span: Span::styled(plain_buf.clone(), base_style),
                        plain: plain_buf.clone(),
                    });
                    plain_buf.clear();
                }

                spans.push(InlineSpan {
                span: Span::styled(
                    url.clone(),
                    Style::default()
                        .fg(theme.link)
                        .add_modifier(Modifier::UNDERLINED),
                ),
                plain: url,
            });

                i = end;
                continue;
            }
        }
```

**Important:** This must go **after** the backtick check (backticks have highest priority — a URL inside backticks should render as code, not a link). Place it between the backtick block and the link `[` block, so the priority is: backtick > bare URL > markdown link > emphasis > plain.

Actually, the backtick check is first because it checks `ch == '`'`. Since bare URLs start with `h`, they won't conflict. But to maintain the documented priority order (backtick > everything else), place the bare URL check **after** the backtick block and **before** the `[` link check.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ui::markdown::tests::bare_url 2>&1 | tail -25`
Expected: all `bare_url_*` tests PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test 2>&1 | tail -10`
Expected: all tests PASS (no regressions)

- [ ] **Step 6: Run clippy**

Run: `cargo clippy 2>&1 | tail -10`
Expected: no new warnings

- [ ] **Step 7: Commit**

```bash
git add src/ui/markdown.rs
git commit -m "feat: detect bare URLs in markdown and style as clickable links"
```

---

### Task 3: Fix markdown link rendering for terminal auto-detection

**Files:**
- Modify: `src/ui/markdown.rs` — link handling in `parse_inline_spans()` (~lines 155-167) and test block

- [ ] **Step 1: Update existing link test and add new tests**

The existing `link_basic` test (line 722) checks `result.plain == "see docs here"` and looks for a span with content `"docs"`. This test needs updating because `plain` will now include the URL.

Replace the existing `link_basic` test and add new tests:

```rust
#[test]
fn link_basic() {
    let result = render_markdown_line("see [docs](https://example.com) here", &dark(), 80);
    // plain now includes the URL for correct wrapping width
    assert_eq!(result.plain, "see docs https://example.com here");
    // Link text span
    let link_span = result
        .styled
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "docs")
        .unwrap();
    assert_eq!(link_span.style.fg, Some(dark().link));
    assert!(link_span.style.add_modifier.contains(Modifier::UNDERLINED));
    // URL span — clean, no parens
    let url_span = result
        .styled
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "https://example.com")
        .unwrap();
    assert_eq!(url_span.style.fg, Some(dark().dim));
    assert!(url_span.style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn link_url_is_contiguous_span() {
    let result = render_markdown_line("[click](https://example.com/path)", &dark(), 80);
    // The URL must be a single contiguous span with no wrapping chars
    let url_span = result
        .styled
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "https://example.com/path")
        .expect("URL should be a clean contiguous span");
    assert!(url_span.style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn link_preserves_raw() {
    let result =
        render_markdown_line("see [docs](https://example.com) here", &dark(), 80);
    assert_eq!(result.raw, "see [docs](https://example.com) here");
}
```

Also update the `raw_preserves_link_syntax` test (~line 969) to match the new `plain`:

```rust
#[test]
fn raw_preserves_link_syntax() {
    let result = render_markdown_line("see [docs](https://example.com)", &dark(), 80);
    assert_eq!(result.raw, "see [docs](https://example.com)");
    assert_eq!(result.plain, "see docs https://example.com");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ui::markdown::tests::link 2>&1 | tail -25`
Expected: FAIL — `plain` still has old format, URL span still wrapped in parens

- [ ] **Step 3: Update link rendering in `parse_inline_spans`**

In `src/ui/markdown.rs`, replace the link handling block (lines 155-167) with:

```rust
                spans.push(InlineSpan {
                    span: Span::styled(
                        link_text.clone(),
                        Style::default()
                            .fg(theme.link)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                    plain: link_text,
                });
                spans.push(InlineSpan {
                    span: Span::styled(" ", base_style),
                    plain: " ".to_string(),
                });
                spans.push(InlineSpan {
                    span: Span::styled(
                        url.clone(),
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                    plain: url,
                });
```

Key changes from old code:
- URL span has **no parentheses** — clean URL text only
- URL span is **underlined** — visual affordance that it's clickable
- Separator is a plain space (not ` (`)
- `plain` for the URL span is the URL itself (was previously empty) — correct wrapping width

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ui::markdown::tests::link 2>&1 | tail -25`
Expected: all `link_*` tests PASS

- [ ] **Step 5: Run full test suite and clippy**

Run: `cargo test 2>&1 | tail -10 && cargo clippy 2>&1 | tail -10`
Expected: all tests PASS, no clippy warnings

- [ ] **Step 6: Commit**

```bash
git add src/ui/markdown.rs
git commit -m "feat: render markdown link URLs as clean spans for terminal auto-detection

Remove parentheses wrapping from URL display so terminals can
detect and make URLs cmd-clickable. URL now rendered as a separate
underlined span with no surrounding punctuation."
```

---

### Task 4: Verify no regressions and manual test

**Files:** None (verification only)

- [ ] **Step 1: Run full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all tests PASS

- [ ] **Step 2: Run clippy**

Run: `cargo clippy 2>&1 | tail -10`
Expected: no warnings

- [ ] **Step 3: Build release**

Run: `cargo build 2>&1 | tail -5`
Expected: compiles successfully

- [ ] **Step 4: Manual smoke test instructions**

Launch Steve and send a message that triggers the LLM to output URLs. Verify:

1. Bare URLs like `https://example.com` appear underlined in link color
2. Markdown links like `[docs](https://example.com)` show "docs" in link color + "https://example.com" underlined in dim
3. Cmd-click (or ctrl-click) on the URL text opens the browser
4. Line wrapping near URLs doesn't break layout
5. Text selection across URLs works correctly (no escape chars in copied text)
