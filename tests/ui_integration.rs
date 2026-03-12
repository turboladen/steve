//! Integration tests for UI features.
//!
//! Exercises the public API to construct UI state, render via ratatui's
//! `TestBackend`, and assert on the rendered output. Covers input wrapping,
//! multi-line paste, copy-on-select, question dialog, syntax highlighting,
//! and autocomplete navigation.

use std::path::PathBuf;

use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    layout::Rect,
    style::Color,
    Frame,
};

use steve::config::types::Config;
use steve::project::ProjectInfo;
use steve::storage::Storage;
use steve::app::App;
use steve::ui::autocomplete::AutocompleteState;
use steve::ui::input::{InputState, InputContext, MIN_INPUT_HEIGHT, MAX_INPUT_PCT};
use steve::ui::message_area::{MessageAreaState, render_message_blocks};
use steve::ui::message_block::{AssistantPart, MessageBlock};
use steve::ui::selection::{ContentMap, ContentPos, SelectionState};
use steve::ui::theme::Theme;
use steve::ui::input::render_input;
use steve::ui::autocomplete::render_autocomplete;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Mirror of `make_test_app()` from `app.rs` using only public APIs.
fn make_test_app() -> App {
    let project = ProjectInfo {
        root: PathBuf::from("/tmp/test"),
        id: "test-ui-integration".to_string(),
    };
    let config = Config::default();
    let storage = Storage::new("test-ui-integration").expect("test storage");
    let usage_writer = steve::usage::test_usage_writer();
    App::new(project, config, storage, None, None, None, Vec::new(), usage_writer)
}

/// Render a draw closure into a headless test buffer.
fn render_to_buffer(w: u16, h: u16, draw: impl FnOnce(&mut Frame)) -> Buffer {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f)).unwrap();
    terminal.backend().buffer().clone()
}

/// Extract plain text from a buffer region (row by row, trimmed trailing spaces).
fn buffer_text(buf: &Buffer, area: Rect) -> String {
    let mut out = String::new();
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        for x in area.x..area.x + area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// Full-buffer text extraction (convenience).
fn full_buffer_text(buf: &Buffer) -> String {
    let area = Rect::new(0, 0, buf.area().width, buf.area().height);
    buffer_text(buf, area)
}

// ===========================================================================
// 1. Input Wrapping & Dynamic Height
// ===========================================================================

#[test]
fn desired_height_increases_with_long_text() {
    let mut input = InputState::default();
    // Insert enough text to wrap at width 40
    let long_line = "a".repeat(120); // 3 visual lines at width 40
    input.textarea.insert_str(&long_line);

    let max_h = 20u16;
    let width = 40u16;
    let height = input.desired_height(max_h, width);

    // 120 chars at width 40 = 3 visual rows, plus overhead (2) = 5 minimum
    assert!(
        height >= MIN_INPUT_HEIGHT,
        "height ({height}) should be >= MIN_INPUT_HEIGHT ({MIN_INPUT_HEIGHT})"
    );
    // With 3 wrapped rows the desired height should exceed the minimum 5
    // (because 2 overhead + 3 content rows = 5, but cursor at EOL adds +1 = 6)
    assert!(
        height > MIN_INPUT_HEIGHT,
        "120 chars at width 40 should need more than minimum height, got {height}"
    );
}

#[test]
fn desired_height_capped_at_max() {
    let mut input = InputState::default();
    // Insert many lines to exceed any reasonable max
    let many_lines = "line\n".repeat(100);
    input.textarea.insert_str(&many_lines);

    let terminal_height = 40u16;
    let max_h = ((terminal_height as u32 * MAX_INPUT_PCT as u32 / 100) as u16).max(MIN_INPUT_HEIGHT);
    let height = input.desired_height(max_h, 80);

    assert!(
        height <= max_h,
        "height ({height}) should be <= max ({max_h})"
    );
}

#[test]
fn desired_height_minimum_for_empty_input() {
    let input = InputState::default();
    let height = input.desired_height(20, 80);
    assert_eq!(height, MIN_INPUT_HEIGHT, "empty input should use minimum height");
}

#[test]
fn input_renders_long_text_wrapped() {
    let mut input = InputState::default();
    let long_text = "The quick brown fox jumps over the lazy dog and keeps running";
    input.textarea.insert_str(long_text);

    let width = 30u16;
    let height = 8u16;
    let theme = Theme::default();
    let ctx = InputContext {
        working_dir: "~/test".to_string(),
        last_prompt_tokens: 0,
        context_window: 128000,
        context_usage_pct: 0,
    };

    let buf = render_to_buffer(width, height, |frame| {
        render_input(
            frame,
            Rect::new(0, 0, width, height),
            &mut input,
            &theme,
            &ctx,
        );
    });

    let text = full_buffer_text(&buf);
    // The text should appear somewhere in the rendered output
    assert!(text.contains("quick"), "rendered input should contain the typed text, got:\n{text}");
    assert!(text.contains("brown"), "wrapped text should also appear, got:\n{text}");
}

// ===========================================================================
// 2. Multi-line Paste Summary
// ===========================================================================

#[test]
fn collapse_paste_creates_summary() {
    let mut input = InputState::default();
    let pasted = "line one\nline two\nline three\nline four\nline five";
    input.collapse_paste(pasted);

    assert!(
        input.collapsed_paste.is_some(),
        "collapsed_paste should be Some after multi-line paste"
    );

    let collapsed = input.collapsed_paste.as_ref().unwrap();
    assert!(
        collapsed.summary.contains("5 lines"),
        "summary should mention line count, got: {}",
        collapsed.summary
    );
    assert!(
        collapsed.summary.starts_with('[') && collapsed.summary.ends_with(']'),
        "summary should be bracketed, got: {}",
        collapsed.summary
    );
}

#[test]
fn collapse_paste_preserves_full_text() {
    let mut input = InputState::default();
    let pasted = "alpha\nbeta\ngamma";
    input.collapse_paste(pasted);

    let collapsed = input.collapsed_paste.as_ref().unwrap();
    assert_eq!(collapsed.full_text, pasted, "full_text should preserve original pasted content");
}

#[test]
fn expand_paste_restores_content() {
    let mut input = InputState::default();
    let pasted = "line1\nline2\nline3";
    input.collapse_paste(pasted);
    assert!(input.collapsed_paste.is_some());

    input.expand_paste();
    assert!(
        input.collapsed_paste.is_none(),
        "collapsed_paste should be None after expand"
    );

    // The textarea should now contain the original text
    let content: String = input.textarea.lines().join("\n");
    assert_eq!(content, pasted, "textarea should contain original pasted text after expand");
}

#[test]
fn single_line_paste_does_not_collapse() {
    let mut input = InputState::default();
    input.collapse_paste("just one line");
    assert!(
        input.collapsed_paste.is_none(),
        "single-line paste should not create collapsed state"
    );
}

#[test]
fn collapse_paste_summary_uses_kb_for_large_pastes() {
    let mut input = InputState::default();
    // Create a paste > 1024 bytes
    let large_paste = "x".repeat(500) + "\n" + &"y".repeat(600);
    input.collapse_paste(&large_paste);

    let collapsed = input.collapsed_paste.as_ref().unwrap();
    assert!(
        collapsed.summary.contains("kb"),
        "large paste summary should use kb unit, got: {}",
        collapsed.summary
    );
}

#[test]
fn collapsed_paste_renders_summary_in_input() {
    let mut input = InputState::default();
    let pasted = "line1\nline2\nline3\nline4";
    input.collapse_paste(pasted);

    let width = 50u16;
    let height = 6u16;
    let theme = Theme::default();
    let ctx = InputContext {
        working_dir: "~/test".to_string(),
        last_prompt_tokens: 0,
        context_window: 128000,
        context_usage_pct: 0,
    };

    let buf = render_to_buffer(width, height, |frame| {
        render_input(
            frame,
            Rect::new(0, 0, width, height),
            &mut input,
            &theme,
            &ctx,
        );
    });

    let text = full_buffer_text(&buf);
    assert!(
        text.contains("4 lines"),
        "rendered input should show collapsed summary with line count, got:\n{text}"
    );
}

// ===========================================================================
// 3. Copy-on-Select Coordinate Mapping
// ===========================================================================

#[test]
fn content_map_build_computes_wrapped_rows() {
    // display_width = GUTTER_WIDTH(3) + text.width(), wrapped at available_width(20)
    let lines = vec![
        "Hello world".to_string(),      // display_width = 3+11 = 14, ceil(14/20) = 1 row
        "Short".to_string(),             // display_width = 3+5 = 8, ceil(8/20) = 1 row
        "".to_string(),                  // empty → 1 row
    ];
    let map = ContentMap::build(lines, 20);

    assert_eq!(map.total_wrapped_rows, 3, "3 short lines should produce 3 wrapped rows");
    assert_eq!(map.wrapped_row_start.len(), 3);
    assert_eq!(map.wrapped_row_start[0], 0);
    assert_eq!(map.wrapped_row_start[1], 1);
    assert_eq!(map.wrapped_row_start[2], 2);
}

#[test]
fn content_map_long_line_wraps() {
    // display_width = GUTTER_WIDTH(3) + text.width(), wrapped at available_width(20)
    // "abcdefghijklmnopqrstuvwxyz": display_width = 3+26 = 29, ceil(29/20) = 2 rows
    // "short": display_width = 3+5 = 8, ceil(8/20) = 1 row
    // Total = 3 rows
    let lines = vec![
        "abcdefghijklmnopqrstuvwxyz".to_string(),
        "short".to_string(),
    ];
    let map = ContentMap::build(lines, 20);

    assert_eq!(map.total_wrapped_rows, 3, "long line (2 rows) + short line (1 row) = 3 total");
    assert_eq!(map.wrapped_row_start[1], 2, "second line starts after first line's 2 wrapped rows");
}

#[test]
fn screen_to_content_basic_mapping() {
    let lines = vec![
        "Hello".to_string(),
        "World".to_string(),
    ];
    let map = ContentMap::build(lines, 40);

    // Row 0, col 3 (gutter) → line 0, char 0
    let pos = map.screen_to_content(0, 3, 0, 0, 0);
    assert_eq!(pos, Some(ContentPos { line: 0, char_offset: 0 }));

    // Row 0, col 5 → line 0, char 2 (col 5 - gutter 3 = content col 2)
    let pos = map.screen_to_content(0, 5, 0, 0, 0);
    assert_eq!(pos, Some(ContentPos { line: 0, char_offset: 2 }));

    // Row 1, col 3 → line 1, char 0
    let pos = map.screen_to_content(1, 3, 0, 0, 0);
    assert_eq!(pos, Some(ContentPos { line: 1, char_offset: 0 }));
}

#[test]
fn screen_to_content_with_scroll_offset() {
    let lines = vec![
        "Line A".to_string(),
        "Line B".to_string(),
        "Line C".to_string(),
    ];
    let map = ContentMap::build(lines, 40);

    // With scroll_offset=1, screen row 0 maps to content row 1 → line 1
    let pos = map.screen_to_content(0, 3, 1, 0, 0);
    assert_eq!(pos, Some(ContentPos { line: 1, char_offset: 0 }));
}

#[test]
fn screen_to_content_in_gutter_gives_offset_zero() {
    let lines = vec!["Hello".to_string()];
    let map = ContentMap::build(lines, 40);

    // Column 0 (in gutter) → char_offset should be 0 (not None)
    let pos = map.screen_to_content(0, 0, 0, 0, 0);
    assert_eq!(pos, Some(ContentPos { line: 0, char_offset: 0 }));
}

#[test]
fn extract_text_single_line() {
    let lines = vec!["Hello World".to_string()];
    let map = ContentMap::build(lines, 80);

    let start = ContentPos { line: 0, char_offset: 0 };
    let end = ContentPos { line: 0, char_offset: 5 };
    assert_eq!(map.extract_text(&start, &end), "Hello");
}

#[test]
fn extract_text_multi_line() {
    let lines = vec![
        "First line".to_string(),
        "Second line".to_string(),
        "Third line".to_string(),
    ];
    let map = ContentMap::build(lines, 80);

    let start = ContentPos { line: 0, char_offset: 6 };
    let end = ContentPos { line: 2, char_offset: 5 };
    let text = map.extract_text(&start, &end);
    assert_eq!(text, "line\nSecond line\nThird");
}

#[test]
fn extract_text_reversed_range_is_normalized() {
    let lines = vec!["Hello World".to_string()];
    let map = ContentMap::build(lines, 80);

    // Pass end before start — should still extract "Hello"
    let start = ContentPos { line: 0, char_offset: 5 };
    let end = ContentPos { line: 0, char_offset: 0 };
    assert_eq!(map.extract_text(&start, &end), "Hello");
}

#[test]
fn extract_text_clamps_offset_to_line_length() {
    let lines = vec!["Hi".to_string()];
    let map = ContentMap::build(lines, 80);

    let start = ContentPos { line: 0, char_offset: 0 };
    let end = ContentPos { line: 0, char_offset: 100 }; // way past end
    assert_eq!(map.extract_text(&start, &end), "Hi");
}

// ===========================================================================
// 4. Question Dialog Rendering
// ===========================================================================

#[test]
fn question_renders_with_options_and_pointer() {
    let messages = vec![MessageBlock::Question {
        question: "Pick a color".to_string(),
        options: vec!["Red".to_string(), "Blue".to_string(), "Green".to_string()],
        selected: Some(0),
        free_text: String::new(),
        answered: None,
    }];

    let width = 60u16;
    let height = 15u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    let text = full_buffer_text(&buf);
    assert!(text.contains("Pick a color"), "question text should appear, got:\n{text}");
    // The selected pointer (▸) should appear on option 1
    assert!(text.contains("\u{25b8}"), "selected pointer (▸) should appear, got:\n{text}");
    assert!(text.contains("Red"), "option 'Red' should appear");
    assert!(text.contains("Blue"), "option 'Blue' should appear");
    assert!(text.contains("Green"), "option 'Green' should appear");
}

#[test]
fn question_pointer_moves_with_selection() {
    // Render with selection at index 1
    let messages = vec![MessageBlock::Question {
        question: "Pick one".to_string(),
        options: vec!["Alpha".to_string(), "Beta".to_string()],
        selected: Some(1),
        free_text: String::new(),
        answered: None,
    }];

    let width = 60u16;
    let height = 12u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    let text = full_buffer_text(&buf);
    // ▸ should be on the "Beta" line, not the "Alpha" line
    for line in text.lines() {
        if line.contains("Alpha") {
            assert!(
                !line.contains("\u{25b8}"),
                "Alpha line should NOT have pointer when Beta is selected: {line}"
            );
        }
        if line.contains("Beta") {
            assert!(
                line.contains("\u{25b8}"),
                "Beta line should have pointer when selected: {line}"
            );
        }
    }
}

#[test]
fn question_answered_shows_arrow() {
    let messages = vec![MessageBlock::Question {
        question: "Pick one".to_string(),
        options: vec!["Yes".to_string(), "No".to_string()],
        selected: Some(0),
        free_text: String::new(),
        answered: Some("Yes".to_string()),
    }];

    let width = 60u16;
    let height = 10u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    let text = full_buffer_text(&buf);
    // Answered state shows → prefix
    assert!(
        text.contains("\u{2192}"),
        "answered question should show → prefix, got:\n{text}"
    );
    assert!(text.contains("Yes"), "answer text should appear");
    // Options should NOT appear when answered
    assert!(
        !text.contains("\u{25b8}"),
        "pointer should not appear when question is answered"
    );
}

#[test]
fn question_free_text_shows_cursor() {
    let messages = vec![MessageBlock::Question {
        question: "What is your name?".to_string(),
        options: vec!["Option A".to_string()],
        selected: None, // free-text mode
        free_text: "typed so far".to_string(),
        answered: None,
    }];

    let width = 60u16;
    let height = 12u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    let text = full_buffer_text(&buf);
    assert!(
        text.contains("typed so far"),
        "free text input should appear, got:\n{text}"
    );
    // The cursor character ▏ (U+258F) should appear
    assert!(
        text.contains("\u{258f}"),
        "cursor character should appear in free-text mode, got:\n{text}"
    );
}

// ===========================================================================
// 5. Syntax Highlighting in Code Blocks
// ===========================================================================

#[test]
fn code_block_renders_language_header() {
    let messages = vec![MessageBlock::Assistant {
        thinking: None,
        parts: vec![AssistantPart::Text(
            "Here is code:\n```rust\nfn main() {}\n```\nDone.".to_string(),
        )],
    }];

    let width = 60u16;
    let height = 12u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    let text = full_buffer_text(&buf);
    assert!(text.contains("rust"), "language header 'rust' should appear, got:\n{text}");
    assert!(text.contains("fn main"), "code content should appear, got:\n{text}");
    assert!(text.contains("Done"), "text after code block should appear, got:\n{text}");
}

#[test]
fn code_block_has_code_bg_background() {
    let messages = vec![MessageBlock::Assistant {
        thinking: None,
        parts: vec![AssistantPart::Text("```rust\nlet x = 42;\n```".to_string())],
    }];

    let width = 60u16;
    let height = 8u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    // Find the row containing "let x = 42" and verify it has code_bg
    let expected_bg = Color::Rgb(28, 26, 23); // theme.code_bg for dark theme
    let mut found_code_line = false;
    for y in 0..height {
        let mut row_text = String::new();
        for x in 0..width {
            row_text.push_str(buf[(x, y)].symbol());
        }
        if row_text.contains("let x") {
            found_code_line = true;
            // Check that at least one cell has code_bg background
            let has_code_bg = (0..width).any(|x| buf[(x, y)].bg == expected_bg);
            assert!(
                has_code_bg,
                "code line should have code_bg ({expected_bg:?}) background on row {y}"
            );
        }
    }
    assert!(found_code_line, "should find a row containing 'let x'");
}

#[test]
fn unknown_language_still_renders_with_code_bg() {
    let messages = vec![MessageBlock::Assistant {
        thinking: None,
        parts: vec![AssistantPart::Text(
            "```nonexistent_language_xyz\nsome code\n```".to_string(),
        )],
    }];

    let width = 60u16;
    let height = 8u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    let text = full_buffer_text(&buf);
    assert!(text.contains("some code"), "code content should render even with unknown language");

    // Verify code_bg is still applied
    let expected_bg = Color::Rgb(28, 26, 23);
    let mut found = false;
    for y in 0..height {
        let mut row_text = String::new();
        for x in 0..width {
            row_text.push_str(buf[(x, y)].symbol());
        }
        if row_text.contains("some code") {
            found = true;
            let has_bg = (0..width).any(|x| buf[(x, y)].bg == expected_bg);
            assert!(has_bg, "unknown language code should still get code_bg");
        }
    }
    assert!(found, "should find code content row");
}

#[test]
fn bare_fence_no_header() {
    let messages = vec![MessageBlock::Assistant {
        thinking: None,
        parts: vec![AssistantPart::Text("```\njust code\n```".to_string())],
    }];

    let width = 60u16;
    let height = 8u16;
    let theme = Theme::default();
    let mut state = MessageAreaState::default();
    let buf = render_to_buffer(width, height, |frame| {
        render_message_blocks(
            frame,
            Rect::new(0, 0, width, height),
            &messages,
            &mut state,
            &theme,
            None,
            0,
            &SelectionState::default(),
        );
    });

    let text = full_buffer_text(&buf);
    assert!(text.contains("just code"), "code should render");

    // Count non-empty content lines. Bare fence now emits:
    // header rule + code + closing rule = 3 content lines.
    let content_lines: Vec<&str> = text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        content_lines.len(), 3,
        "bare fence should produce 3 content lines (header rule + code + closing rule), got: {content_lines:?}"
    );
}

// ===========================================================================
// 6. Autocomplete Navigation
// ===========================================================================

#[test]
fn autocomplete_update_matches_commands() {
    let mut ac = AutocompleteState::default();
    ac.update("/m");

    assert!(ac.visible, "autocomplete should be visible for '/m' prefix");
    // /model and /models should both match
    let cmd = ac.selected_command();
    assert!(cmd.is_some(), "should have a selected command");
}

#[test]
fn autocomplete_next_prev_wraps() {
    let mut ac = AutocompleteState::default();
    ac.update("/");  // match all commands

    assert!(ac.visible, "autocomplete should be visible for '/'");
    let initial = ac.selected;
    assert_eq!(initial, 0, "initial selection should be 0");

    ac.next();
    assert_eq!(ac.selected, 1, "next() should advance to 1");

    // Go back
    ac.prev();
    assert_eq!(ac.selected, 0, "prev() should return to 0");

    // Wrap backwards from 0
    ac.prev();
    assert!(ac.selected > 0, "prev() from 0 should wrap to last item");
}

#[test]
fn autocomplete_selected_command_returns_name() {
    let mut ac = AutocompleteState::default();
    ac.update("/he");

    assert!(ac.visible, "autocomplete should be visible for '/he' (matches /help)");
    let cmd = ac.selected_command().unwrap();
    assert_eq!(cmd, "/help", "selected command should be '/help'");
}

#[test]
fn autocomplete_hides_on_non_command_input() {
    let mut ac = AutocompleteState::default();
    ac.update("/m");
    assert!(ac.visible);

    ac.update("hello world");
    assert!(!ac.visible, "autocomplete should hide for non-command input");
}

#[test]
fn autocomplete_hides_on_command_with_space() {
    let mut ac = AutocompleteState::default();
    ac.update("/model openai");
    assert!(!ac.visible, "autocomplete should hide when command has arguments");
}

#[test]
fn autocomplete_renders_selected_with_accent() {
    let mut ac = AutocompleteState::default();
    ac.update("/");
    assert!(ac.visible, "autocomplete should be visible for '/'");

    let theme = Theme::default();
    let width = 60u16;
    let height = 30u16;

    // Position autocomplete above a fake input area near the bottom
    let input_area = Rect::new(0, height - 5, width, 5);

    let buf = render_to_buffer(width, height, |frame| {
        render_autocomplete(frame, input_area, &ac, &theme, 0);
    });

    // Find a cell with the accent color (the selected item)
    let accent = theme.accent;
    let mut found_accent = false;
    for y in 0..height {
        for x in 0..width {
            if buf[(x, y)].fg == accent {
                found_accent = true;
                break;
            }
        }
        if found_accent {
            break;
        }
    }
    assert!(found_accent, "selected autocomplete item should use accent color ({accent:?})");
}

#[test]
fn autocomplete_next_changes_highlighted_item() {
    let mut ac = AutocompleteState::default();
    ac.update("/");

    let first_cmd = ac.selected_command().map(|s| s.to_string());
    ac.next();
    let second_cmd = ac.selected_command().map(|s| s.to_string());

    assert_ne!(
        first_cmd, second_cmd,
        "next() should change the selected command"
    );
}

// ===========================================================================
// Full-app render smoke test
// ===========================================================================

#[test]
fn full_app_renders_without_panic() {
    let mut app = make_test_app();
    app.messages.push(MessageBlock::User {
        text: "Hello, Steve!".to_string(),
    });
    app.messages.push(MessageBlock::Assistant {
        thinking: None,
        parts: vec![AssistantPart::Text("Hi there!".to_string())],
    });

    let width = 80u16;
    let height = 24u16;
    let buf = render_to_buffer(width, height, |frame| {
        steve::ui::render(frame, &mut app);
    });

    let text = full_buffer_text(&buf);
    assert!(text.contains("Hello, Steve!"), "user message should render");
    assert!(text.contains("Hi there!"), "assistant message should render");
}

#[test]
fn full_app_wide_renders_sidebar() {
    let mut app = make_test_app();

    let width = 140u16; // wide enough for sidebar (>= 120)
    let height = 30u16;
    let buf = render_to_buffer(width, height, |frame| {
        steve::ui::render(frame, &mut app);
    });

    let text = full_buffer_text(&buf);
    // Sidebar always shows "Session" section regardless of state
    assert!(
        text.contains("Session"),
        "wide terminal should show sidebar with Session section, got:\n{text}"
    );
}
