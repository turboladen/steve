use std::time::Instant;
use unicode_width::UnicodeWidthStr;

/// A position within the logical content (pre-wrapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentPos {
    /// Index into `ContentMap::line_texts`.
    pub line: usize,
    /// Character offset within that line (0-based, clamped to line length).
    pub char_offset: usize,
}

impl ContentPos {
    /// Ordering comparison: returns true if self comes before other in document order.
    fn before(&self, other: &ContentPos) -> bool {
        self.line < other.line || (self.line == other.line && self.char_offset < other.char_offset)
    }
}

/// State for click-drag text selection in the message area.
#[derive(Default)]
pub struct SelectionState {
    /// Mouse-down position (start of selection).
    pub anchor: Option<ContentPos>,
    /// Current drag position (end of selection).
    pub cursor: Option<ContentPos>,
    /// Whether we are currently tracking a drag.
    pub dragging: bool,
    /// When a successful copy happened (for "Copied!" flash).
    pub copied_flash: Option<Instant>,
}

impl SelectionState {
    /// Returns the selection range in document order (start, end), or None if no selection.
    pub fn ordered_range(&self) -> Option<(ContentPos, ContentPos)> {
        match (self.anchor, self.cursor) {
            (Some(a), Some(c)) if a != c => {
                if a.before(&c) {
                    Some((a, c))
                } else {
                    Some((c, a))
                }
            }
            _ => None,
        }
    }

    /// Clear the selection state entirely.
    pub fn clear(&mut self) {
        self.anchor = None;
        self.cursor = None;
        self.dragging = false;
    }
}

/// Width of the activity rail gutter in columns.
/// Must match `GUTTER_WIDTH` in `message_area.rs`.
const GUTTER_WIDTH: usize = 3;

/// A parallel plain-text representation of the rendered message area content,
/// built during render to enable screen coordinate → content position mapping.
pub struct ContentMap {
    /// Plain text for each logical line (gutter-stripped).
    /// Matches visual display width — used for wrapping math and coordinate mapping.
    pub line_texts: Vec<String>,
    /// Raw markdown source for each logical line, parallel to `line_texts`.
    /// Used for clipboard extraction — preserves original markdown syntax.
    pub raw_line_texts: Vec<String>,
    /// Cumulative wrapped row index where each logical line starts.
    /// `wrapped_row_start[i]` = sum of wrapped rows for lines 0..i.
    pub wrapped_row_start: Vec<u32>,
    /// Total number of wrapped rows across all lines.
    pub total_wrapped_rows: u32,
    /// Available width for content (terminal width minus gutter, used for wrapping math).
    pub available_width: usize,
}

impl ContentMap {
    /// Build a ContentMap from line texts, raw markdown texts, and the available width.
    ///
    /// `line_texts` must match visual display widths (for wrapping calculations).
    /// `raw_line_texts` preserves original markdown source (for clipboard copy).
    pub fn build(line_texts: Vec<String>, raw_line_texts: Vec<String>, full_width: usize) -> Self {
        debug_assert_eq!(line_texts.len(), raw_line_texts.len());
        let available_width = full_width.max(1);
        let mut wrapped_row_start = Vec::with_capacity(line_texts.len());
        let mut cumulative: u32 = 0;

        for text in &line_texts {
            wrapped_row_start.push(cumulative);
            let line_width = Self::display_width(text, available_width);
            let rows = if line_width == 0 {
                1u32
            } else {
                line_width.div_ceil(available_width) as u32
            };
            cumulative += rows;
        }

        Self {
            line_texts,
            raw_line_texts,
            wrapped_row_start,
            total_wrapped_rows: cumulative,
            available_width,
        }
    }

    /// Compute the display width of a line text in the same way ratatui would.
    ///
    /// The gutter is prepended before wrapping, so the full rendered line is
    /// `GUTTER_WIDTH + text_display_width`. This must match how `render_message_blocks`
    /// computes `line.width()` — which uses unicode display width (CJK = 2 columns).
    fn display_width(text: &str, _available_width: usize) -> usize {
        GUTTER_WIDTH + text.width()
    }

    /// Map screen coordinates to a content position.
    ///
    /// `row` and `col` are terminal-relative (0-based from frame origin).
    /// `scroll_offset` is the current scroll position.
    /// `area_y` and `area_x` are the top-left corner of the message area.
    ///
    /// Returns `None` if the position is outside content bounds or in the gutter.
    pub fn screen_to_content(
        &self,
        row: u16,
        col: u16,
        scroll_offset: u16,
        area_y: u16,
        area_x: u16,
    ) -> Option<ContentPos> {
        if self.line_texts.is_empty() {
            return None;
        }

        // Convert screen row to content row (wrapped row index)
        let content_row = (row.saturating_sub(area_y) as u32) + (scroll_offset as u32);

        // Convert screen col to content col (subtract area origin + gutter)
        let raw_col = col.saturating_sub(area_x) as usize;
        // Allow clicks in the gutter to select at char_offset=0
        let content_col = raw_col.saturating_sub(GUTTER_WIDTH);

        // Binary search to find which logical line this wrapped row belongs to
        let line_idx = match self.wrapped_row_start.binary_search(&content_row) {
            Ok(idx) => {
                // Exact match — could be the start of this line or a later line with same start.
                // Find the last line that starts at this row (handles zero-width lines).
                let mut result = idx;
                while result + 1 < self.wrapped_row_start.len()
                    && self.wrapped_row_start[result + 1] == content_row
                {
                    result += 1;
                }
                result
            }
            Err(idx) => {
                // content_row falls between wrapped_row_start[idx-1] and wrapped_row_start[idx]
                if idx == 0 {
                    return None; // before first line
                }
                idx - 1
            }
        };

        if line_idx >= self.line_texts.len() {
            return None;
        }

        // Check if content_row is beyond the last line's wrapped rows
        if content_row >= self.total_wrapped_rows {
            return None;
        }

        // Calculate character offset within this logical line.
        // We work in display columns (unicode width) to match ratatui's wrapping,
        // then convert to a char offset for text extraction.
        let row_within_line = content_row - self.wrapped_row_start[line_idx];
        // Content width is available_width minus gutter
        let content_width = self.available_width.saturating_sub(GUTTER_WIDTH);
        let content_width = content_width.max(1);
        let target_col = (row_within_line as usize) * content_width + content_col;

        // Convert display column to char offset by walking characters
        let text = &self.line_texts[line_idx];
        let mut col_pos = 0usize;
        let mut char_offset = 0usize;
        for ch in text.chars() {
            if col_pos >= target_col {
                break;
            }
            col_pos += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            char_offset += 1;
        }
        // If target_col is past the line end, char_offset is already clamped to line length

        Some(ContentPos {
            line: line_idx,
            char_offset,
        })
    }

    /// Extract text between two content positions (inclusive of start, exclusive of end).
    ///
    /// Fully-selected lines return raw markdown (preserving `#`, `**`, `` ` ``, etc.).
    /// Partially-selected first/last lines return plain text (char offsets are computed
    /// against `line_texts` display widths and don't map to `raw_line_texts`).
    pub fn extract_text(&self, start: &ContentPos, end: &ContentPos) -> String {
        // Ensure start < end
        let (start, end) = if start.before(end) {
            (start, end)
        } else {
            (end, start)
        };

        if start.line == end.line {
            let text = &self.line_texts[start.line];
            let chars: Vec<char> = text.chars().collect();
            let from = start.char_offset.min(chars.len());
            let to = end.char_offset.min(chars.len());
            // Fully selected → raw markdown; partial → plain slice.
            // Use end.char_offset (pre-clamp) to distinguish "selected past end"
            // from "selected nothing" on empty display lines (e.g. code fence
            // closers where plain="" but raw="```").
            if from == 0 && end.char_offset > 0 && to >= chars.len() {
                self.raw_line_texts[start.line].clone()
            } else {
                chars[from..to].iter().collect()
            }
        } else {
            let mut result = String::new();

            // First line: full if starting at offset 0, otherwise partial plain
            let first_chars: Vec<char> = self.line_texts[start.line].chars().collect();
            let from = start.char_offset.min(first_chars.len());
            if from == 0 {
                result.push_str(&self.raw_line_texts[start.line]);
            } else {
                result.extend(&first_chars[from..]);
            }

            // Middle lines: always fully selected → raw markdown
            for line_idx in (start.line + 1)..end.line {
                result.push('\n');
                result.push_str(&self.raw_line_texts[line_idx]);
            }

            // Last line: full if selecting to end, otherwise partial plain.
            // Use end.char_offset (pre-clamp) to handle empty display lines.
            result.push('\n');
            let last_chars: Vec<char> = self.line_texts[end.line].chars().collect();
            let to = end.char_offset.min(last_chars.len());
            if end.char_offset > 0 && to >= last_chars.len() {
                result.push_str(&self.raw_line_texts[end.line]);
            } else {
                result.extend(&last_chars[..to]);
            }

            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a ContentMap where raw == plain (no markdown processing).
    fn build_plain(line_texts: Vec<String>, full_width: usize) -> ContentMap {
        let raws = line_texts.clone();
        ContentMap::build(line_texts, raws, full_width)
    }

    // -- ContentPos ordering --

    #[test]
    fn content_pos_before_same_line() {
        let a = ContentPos {
            line: 0,
            char_offset: 2,
        };
        let b = ContentPos {
            line: 0,
            char_offset: 5,
        };
        assert!(a.before(&b));
        assert!(!b.before(&a));
    }

    #[test]
    fn content_pos_before_different_lines() {
        let a = ContentPos {
            line: 0,
            char_offset: 10,
        };
        let b = ContentPos {
            line: 1,
            char_offset: 0,
        };
        assert!(a.before(&b));
        assert!(!b.before(&a));
    }

    #[test]
    fn content_pos_before_equal() {
        let a = ContentPos {
            line: 2,
            char_offset: 3,
        };
        assert!(!a.before(&a));
    }

    // -- SelectionState --

    #[test]
    fn ordered_range_none_when_no_selection() {
        let state = SelectionState::default();
        assert!(state.ordered_range().is_none());
    }

    #[test]
    fn ordered_range_none_when_anchor_only() {
        let state = SelectionState {
            anchor: Some(ContentPos {
                line: 0,
                char_offset: 0,
            }),
            cursor: None,
            dragging: false,
            copied_flash: None,
        };
        assert!(state.ordered_range().is_none());
    }

    #[test]
    fn ordered_range_none_when_same_position() {
        let pos = ContentPos {
            line: 1,
            char_offset: 5,
        };
        let state = SelectionState {
            anchor: Some(pos),
            cursor: Some(pos),
            dragging: false,
            copied_flash: None,
        };
        assert!(state.ordered_range().is_none());
    }

    #[test]
    fn ordered_range_forward() {
        let a = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let b = ContentPos {
            line: 1,
            char_offset: 5,
        };
        let state = SelectionState {
            anchor: Some(a),
            cursor: Some(b),
            dragging: false,
            copied_flash: None,
        };
        let (start, end) = state.ordered_range().unwrap();
        assert_eq!(start, a);
        assert_eq!(end, b);
    }

    #[test]
    fn ordered_range_reversed() {
        let a = ContentPos {
            line: 2,
            char_offset: 10,
        };
        let b = ContentPos {
            line: 0,
            char_offset: 3,
        };
        let state = SelectionState {
            anchor: Some(a),
            cursor: Some(b),
            dragging: false,
            copied_flash: None,
        };
        let (start, end) = state.ordered_range().unwrap();
        assert_eq!(start, b);
        assert_eq!(end, a);
    }

    #[test]
    fn clear_resets_all() {
        let mut state = SelectionState {
            anchor: Some(ContentPos {
                line: 0,
                char_offset: 0,
            }),
            cursor: Some(ContentPos {
                line: 1,
                char_offset: 5,
            }),
            dragging: true,
            copied_flash: Some(Instant::now()),
        };
        state.clear();
        assert!(state.anchor.is_none());
        assert!(state.cursor.is_none());
        assert!(!state.dragging);
        // copied_flash is NOT cleared by clear() — it's on a timer
    }

    // -- ContentMap building --

    #[test]
    fn build_empty() {
        let map = build_plain(vec![], 80);
        assert_eq!(map.total_wrapped_rows, 0);
        assert!(map.line_texts.is_empty());
    }

    #[test]
    fn build_single_short_line() {
        let map = build_plain(vec!["hello".to_string()], 80);
        assert_eq!(map.total_wrapped_rows, 1);
        assert_eq!(map.wrapped_row_start, vec![0]);
    }

    #[test]
    fn build_empty_line_counts_as_one_row() {
        let map = build_plain(vec!["".to_string()], 80);
        assert_eq!(map.total_wrapped_rows, 1);
    }

    #[test]
    fn build_wrapped_line() {
        // "hello world" = 11 chars + 3 gutter = 14 display width
        // With available_width=10, wraps to ceil(14/10)=2 rows
        let map = build_plain(vec!["hello world".to_string()], 10);
        assert_eq!(map.total_wrapped_rows, 2);
    }

    #[test]
    fn build_multiple_lines_cumulative() {
        let lines = vec![
            "short".to_string(),   // 5+3=8 chars, 1 row at width 80
            "another".to_string(), // 7+3=10 chars, 1 row at width 80
        ];
        let map = build_plain(lines, 80);
        assert_eq!(map.wrapped_row_start, vec![0, 1]);
        assert_eq!(map.total_wrapped_rows, 2);
    }

    // -- screen_to_content --

    #[test]
    fn screen_to_content_basic() {
        let map = build_plain(
            vec!["Hello, world!".to_string(), "Second line".to_string()],
            80,
        );

        // First line, after gutter (col = 0 + 3 gutter = 3)
        let pos = map.screen_to_content(0, 3, 0, 0, 0).unwrap();
        assert_eq!(pos.line, 0);
        assert_eq!(pos.char_offset, 0);

        // First line, 5th char
        let pos = map.screen_to_content(0, 8, 0, 0, 0).unwrap();
        assert_eq!(pos.line, 0);
        assert_eq!(pos.char_offset, 5);
    }

    #[test]
    fn screen_to_content_second_line() {
        let map = build_plain(vec!["Hello".to_string(), "World".to_string()], 80);

        let pos = map.screen_to_content(1, 3, 0, 0, 0).unwrap();
        assert_eq!(pos.line, 1);
        assert_eq!(pos.char_offset, 0);
    }

    #[test]
    fn screen_to_content_with_scroll_offset() {
        let map = build_plain(
            vec![
                "Line 0".to_string(),
                "Line 1".to_string(),
                "Line 2".to_string(),
            ],
            80,
        );

        // Scrolled down 1 row, screen row 0 shows line 1
        let pos = map.screen_to_content(0, 3, 1, 0, 0).unwrap();
        assert_eq!(pos.line, 1);
    }

    #[test]
    fn screen_to_content_with_area_offset() {
        let map = build_plain(vec!["Hello".to_string()], 80);

        // Area starts at (5, 10)
        let pos = map.screen_to_content(10, 8, 0, 10, 5).unwrap();
        assert_eq!(pos.line, 0);
        assert_eq!(pos.char_offset, 0);
    }

    #[test]
    fn screen_to_content_clamps_past_line_end() {
        let map = build_plain(
            vec![
                "Hi".to_string(), // 2 chars
            ],
            80,
        );

        // Column way past end of "Hi"
        let pos = map.screen_to_content(0, 50, 0, 0, 0).unwrap();
        assert_eq!(pos.line, 0);
        assert_eq!(pos.char_offset, 2); // clamped to line length
    }

    #[test]
    fn screen_to_content_gutter_click() {
        let map = build_plain(vec!["Hello".to_string()], 80);

        // Click in gutter area (col < GUTTER_WIDTH)
        let pos = map.screen_to_content(0, 0, 0, 0, 0).unwrap();
        assert_eq!(pos.line, 0);
        assert_eq!(pos.char_offset, 0); // selects at start of line
    }

    #[test]
    fn screen_to_content_beyond_content() {
        let map = build_plain(vec!["Only line".to_string()], 80);

        // Row 5 is way beyond our single line
        let result = map.screen_to_content(5, 3, 0, 0, 0);
        assert!(result.is_none());
    }

    #[test]
    fn screen_to_content_empty_map() {
        let map = build_plain(vec![], 80);
        assert!(map.screen_to_content(0, 0, 0, 0, 0).is_none());
    }

    // -- extract_text --

    #[test]
    fn extract_text_single_line() {
        let map = build_plain(vec!["Hello, world!".to_string()], 80);

        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 5,
        };
        assert_eq!(map.extract_text(&start, &end), "Hello");
    }

    #[test]
    fn extract_text_partial_line() {
        let map = build_plain(vec!["Hello, world!".to_string()], 80);

        let start = ContentPos {
            line: 0,
            char_offset: 7,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 12,
        };
        assert_eq!(map.extract_text(&start, &end), "world");
    }

    #[test]
    fn extract_text_multi_line() {
        let map = build_plain(
            vec![
                "First line".to_string(),
                "Second line".to_string(),
                "Third line".to_string(),
            ],
            80,
        );

        let start = ContentPos {
            line: 0,
            char_offset: 6,
        };
        let end = ContentPos {
            line: 2,
            char_offset: 5,
        };
        assert_eq!(map.extract_text(&start, &end), "line\nSecond line\nThird");
    }

    #[test]
    fn extract_text_reversed_range() {
        let map = build_plain(vec!["Hello, world!".to_string()], 80);

        // Pass end before start — should still work
        let start = ContentPos {
            line: 0,
            char_offset: 7,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 0,
        };
        assert_eq!(map.extract_text(&start, &end), "Hello, ");
    }

    #[test]
    fn extract_text_empty_selection() {
        let map = build_plain(vec!["Hello".to_string()], 80);

        let pos = ContentPos {
            line: 0,
            char_offset: 2,
        };
        assert_eq!(map.extract_text(&pos, &pos), "");
    }

    #[test]
    fn extract_text_clamps_offsets() {
        let map = build_plain(
            vec![
                "Hi".to_string(), // 2 chars
            ],
            80,
        );

        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 100,
        }; // way past end
        assert_eq!(map.extract_text(&start, &end), "Hi");
    }

    #[test]
    fn extract_text_adjacent_lines() {
        let map = build_plain(vec!["AAAA".to_string(), "BBBB".to_string()], 80);

        let start = ContentPos {
            line: 0,
            char_offset: 2,
        };
        let end = ContentPos {
            line: 1,
            char_offset: 2,
        };
        assert_eq!(map.extract_text(&start, &end), "AA\nBB");
    }

    // -- Raw markdown extraction --

    #[test]
    fn extract_text_full_line_returns_raw() {
        let map = ContentMap::build(
            vec!["Hello World".to_string()],
            vec!["# Hello World".to_string()],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 100,
        };
        assert_eq!(map.extract_text(&start, &end), "# Hello World");
    }

    #[test]
    fn extract_text_partial_line_returns_plain() {
        let map = ContentMap::build(
            vec!["Hello World".to_string()],
            vec!["# Hello World".to_string()],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 0,
            char_offset: 5,
        };
        assert_eq!(map.extract_text(&start, &end), "Hello");
    }

    #[test]
    fn extract_text_multi_line_middle_uses_raw() {
        let map = ContentMap::build(
            vec![
                "First".to_string(),
                "bold text".to_string(),
                "Third".to_string(),
            ],
            vec![
                "# First".to_string(),
                "**bold** text".to_string(),
                "### Third".to_string(),
            ],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 2,
            char_offset: 100,
        };
        assert_eq!(
            map.extract_text(&start, &end),
            "# First\n**bold** text\n### Third"
        );
    }

    #[test]
    fn extract_text_partial_first_line_raw_rest() {
        let map = ContentMap::build(
            vec![
                "Hello World".to_string(),
                "bold text".to_string(),
                "Third".to_string(),
            ],
            vec![
                "# Hello World".to_string(),
                "**bold** text".to_string(),
                "### Third".to_string(),
            ],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 6,
        };
        let end = ContentPos {
            line: 2,
            char_offset: 100,
        };
        // First line partial (from plain), middle raw, last full raw
        assert_eq!(
            map.extract_text(&start, &end),
            "World\n**bold** text\n### Third"
        );
    }

    #[test]
    fn extract_text_code_fence_raw_preserved() {
        let map = ContentMap::build(
            vec![
                "rust".to_string(),
                "let x = 1;".to_string(),
                "".to_string(),
            ],
            vec![
                "```rust".to_string(),
                "let x = 1;".to_string(),
                "```".to_string(),
            ],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 2,
            char_offset: 100,
        };
        assert_eq!(
            map.extract_text(&start, &end),
            "```rust\nlet x = 1;\n```"
        );
    }

    #[test]
    fn extract_text_partial_last_line_uses_plain() {
        let map = ContentMap::build(
            vec![
                "First".to_string(),
                "bold text".to_string(),
            ],
            vec![
                "# First".to_string(),
                "**bold** text".to_string(),
            ],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 1,
            char_offset: 4,
        };
        // First line full (raw), last line partial (plain)
        assert_eq!(map.extract_text(&start, &end), "# First\nbold");
    }

    #[test]
    fn extract_text_empty_plain_line_does_not_return_raw() {
        // Code fence closer: plain is "" (rendered as decorative rule) but raw is "```".
        // Selecting TO this line at offset 0 should not emit the raw fence marker.
        let map = ContentMap::build(
            vec![
                "let x = 1;".to_string(),
                "".to_string(), // fence closer plain
            ],
            vec![
                "let x = 1;".to_string(),
                "```".to_string(), // fence closer raw
            ],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 1,
            char_offset: 0,
        };
        // Last line has char_offset=0 on empty plain → should NOT return raw "```"
        assert_eq!(map.extract_text(&start, &end), "let x = 1;\n");
    }

    #[test]
    fn extract_text_empty_plain_line_full_selection_returns_raw() {
        // When an empty-plain line is a middle line (fully selected), raw IS returned.
        let map = ContentMap::build(
            vec![
                "code".to_string(),
                "".to_string(), // fence closer plain
                "next".to_string(),
            ],
            vec![
                "code".to_string(),
                "```".to_string(), // fence closer raw
                "next".to_string(),
            ],
            80,
        );
        let start = ContentPos {
            line: 0,
            char_offset: 0,
        };
        let end = ContentPos {
            line: 2,
            char_offset: 100,
        };
        // Middle line is fully selected → raw "```" included
        assert_eq!(map.extract_text(&start, &end), "code\n```\nnext");
    }
}
