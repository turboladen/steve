pub mod autocomplete;
pub mod diagnostics_overlay;
pub mod input;
pub mod layout;
pub mod markdown;
pub mod mcp_overlay;
pub mod message_area;
pub mod message_block;
pub mod model_picker;
pub mod primitives;
pub mod selection;
pub mod session_picker;
pub mod sidebar;
pub mod status_line;
pub mod syntax;
pub mod terminal_detect;
pub mod theme;

use std::io::{self, Stdout, Write};

use anyhow::Result;
use crossterm::{
    cursor,
    cursor::MoveTo,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute, queue,
    style::Print,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Position, Rect},
    style::Style,
    widgets::Block,
};

use crate::app::App;
use autocomplete::render_autocomplete;
use diagnostics_overlay::render_diagnostics_overlay;
use input::{
    CHEVRON_WIDTH, InputContext, MAX_INPUT_PCT, MIN_INPUT_HEIGHT, abbreviate_path, render_input,
    render_paste_preview,
};
use layout::compute_layout;
use mcp_overlay::render_mcp_overlay;
use message_area::render_message_blocks;
use model_picker::render_model_picker;
use session_picker::render_session_picker;
use sidebar::render_sidebar;
use status_line::Activity;
use std::time::Duration;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Set up the terminal with OSC 11 background detection.
///
/// Enables raw mode first (required for reading the OSC response), then
/// probes the terminal background before entering the alternate screen
/// (which would hide the response).
pub fn detect_and_setup_terminal() -> Result<(Tui, terminal_detect::DetectedBackground)> {
    enable_raw_mode()?;
    let detected = terminal_detect::detect_background();
    tracing::info!(?detected, "terminal background detected");
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok((terminal, detected))
}

pub fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Write OSC 8 hyperlink sequences directly to stdout for URLs in the buffer.
///
/// Called AFTER `terminal.draw()` has flushed the frame to the terminal.
/// Scans the rendered buffer for bare URLs and re-writes them with OSC 8
/// escape sequences wrapping the text. Since this writes directly to stdout
/// (bypassing ratatui's buffer), it doesn't affect width calculations,
/// cell state, or crossterm's buffer diffing.
pub fn write_osc8_hyperlinks(buf: &Buffer, area: Rect) {
    let mut stdout = io::stdout();
    // Save cursor position before writing OSC 8 sequences — these bypass ratatui's
    // buffer and leave the cursor wherever the last URL ended.
    let _ = queue!(stdout, cursor::SavePosition, cursor::Hide);

    for y in area.y..area.y + area.height {
        // Collect chars from cell symbols with mapping to x positions
        let mut chars: Vec<char> = Vec::new();
        let mut char_to_x: Vec<u16> = Vec::new();

        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell(Position::new(x, y)) {
                let sym = cell.symbol();
                for ch in sym.chars() {
                    chars.push(ch);
                    char_to_x.push(x);
                }
            }
        }

        // Find URLs and emit OSC 8 sequences
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == 'h'
                && let Some((url, end)) = markdown::scan_bare_url(&chars, i)
            {
                let first_x = char_to_x[i];
                let last_x = char_to_x[end - 1];

                // Skip URLs at right edge (likely truncated by wrapping)
                if last_x >= area.x + area.width - 1 {
                    i = end;
                    continue;
                }

                // Collect the label from cells
                let label: String = (first_x..=last_x)
                    .filter_map(|x| buf.cell(Position::new(x, y)).map(|c| c.symbol()))
                    .collect();

                // Move to URL start, write OSC 8 open, re-write label, write OSC 8 close
                let _ = queue!(
                    stdout,
                    MoveTo(first_x, y),
                    Print(format!("\x1b]8;;{url}\x1b\\")),
                    Print(&label),
                    Print("\x1b]8;;\x1b\\"),
                );

                i = end;
                continue;
            }
            i += 1;
        }
    }

    let _ = queue!(stdout, cursor::RestorePosition, cursor::Show);
    let _ = stdout.flush();
}

/// Build the OSC 7 escape sequence for the given path.
///
/// Uses `url::Url::from_file_path` to produce a properly percent-encoded
/// `file://` URI (spaces → `%20`, `#` → `%23`, etc.), then grafts the
/// local hostname onto it so terminal emulators can distinguish local
/// from remote sessions.
fn osc7_sequence(cwd: &std::path::Path) -> Option<String> {
    let mut url = url::Url::from_file_path(cwd).ok()?;
    let hostname = gethostname::gethostname();
    let hostname = hostname.to_string_lossy();
    if hostname.is_empty() || url.set_host(Some(&hostname)).is_err() {
        url.set_host(Some("localhost")).ok()?;
    }
    Some(format!("\x1b]7;{url}\x1b\\"))
}

/// Emit OSC 7 to tell the terminal our working directory.
///
/// Called once after entering the alternate screen so new tabs/splits
/// opened from this terminal inherit the project CWD.  Emitting once
/// is sufficient because Steve has no `/cd` command — the project CWD
/// is fixed for the lifetime of the session.
pub fn write_osc7_cwd(cwd: &std::path::Path) {
    if let Some(seq) = osc7_sequence(cwd) {
        let mut stdout = io::stdout();
        let _ = queue!(stdout, Print(seq));
        let _ = stdout.flush();
    }
}

/// Render a widget into a headless test buffer. Used by rendering tests.
#[cfg(test)]
pub(crate) fn render_to_buffer(
    width: u16,
    height: u16,
    draw: impl FnOnce(&mut Frame),
) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f)).unwrap();
    terminal.backend().buffer().clone()
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let show_sidebar = app.should_show_sidebar(area.width);
    let max_input =
        ((area.height as u32 * MAX_INPUT_PCT as u32 / 100) as u16).max(MIN_INPUT_HEIGHT);
    // Compute textarea width accounting for sidebar and chevron
    let content_width = if show_sidebar && area.width >= 120 {
        let sb_width = layout::sidebar_width(area.width);
        area.width.saturating_sub(1 + sb_width) // separator + sidebar
    } else {
        area.width
    };
    let textarea_width = content_width.saturating_sub(CHEVRON_WIDTH);
    let input_height = app.input.desired_height(max_input, textarea_width);
    let layout = compute_layout(area, show_sidebar, input_height);

    // Context pressure percentage — drives ambient border color shifts
    let pct = app.status_line_state.context_usage_pct();

    // Build activity info for inline display in message area
    let has_pending_input = app.is_loading && !app.input.textarea.lines().join("").is_empty();
    let activity: Option<(char, String, bool, Option<Duration>)> = if app.is_loading {
        let state = &app.status_line_state;
        if *state.activity() != Activity::Idle {
            let activity_elapsed = state.activity_start.map(|t| t.elapsed());
            state.spinner_char().map(|ch| {
                (
                    ch,
                    state.activity_text(),
                    has_pending_input,
                    activity_elapsed,
                )
            })
        } else {
            None
        }
    } else {
        None
    };

    // Store message area rect for mouse event hit-testing
    app.last_message_area = layout.message_area;

    render_message_blocks(
        frame,
        layout.message_area,
        &app.messages,
        &mut app.message_area_state,
        &app.theme,
        activity,
        pct,
        &app.selection_state,
    );

    if let Some(sep_area) = layout.sidebar_separator {
        // Render a thin colored column as visual separator — copies as a space, not │
        let sep = Block::default().style(Style::default().bg(app.theme.border_color(pct)));
        frame.render_widget(sep, sep_area);
    }

    if let Some(sidebar_area) = layout.sidebar {
        render_sidebar(frame, sidebar_area, &app.sidebar_state, &app.theme, pct);
    }

    let context = InputContext {
        working_dir: abbreviate_path(&app.project.root),
        last_prompt_tokens: app.status_line_state.last_prompt_tokens,
        context_window: app.status_line_state.context_window,
        context_usage_pct: pct,
        elapsed: app
            .frozen_elapsed
            .or_else(|| app.stream_start_time.map(|t| t.elapsed())),
    };

    render_input(
        frame,
        layout.input_area,
        &mut app.input,
        &app.theme,
        &context,
    );

    render_autocomplete(
        frame,
        layout.input_area,
        &app.autocomplete_state,
        &app.theme,
        pct,
    );

    render_model_picker(
        frame,
        layout.message_area,
        &app.model_picker,
        &app.theme,
        pct,
    );

    render_session_picker(
        frame,
        layout.message_area,
        &app.session_picker,
        &app.theme,
        pct,
    );

    render_diagnostics_overlay(
        frame,
        layout.message_area,
        &app.diagnostics_overlay,
        &app.theme,
        pct,
    );

    render_mcp_overlay(
        frame,
        layout.message_area,
        &app.mcp_overlay,
        &app.theme,
        pct,
    );

    render_paste_preview(frame, layout.message_area, &app.input, &app.theme, pct);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: render the full app into a buffer and return text.
    fn render_app_to_parts(width: u16, height: u16) -> (ratatui::buffer::Buffer, String) {
        let mut app = crate::app::tests::make_test_app();
        let buf = render_to_buffer(width, height, |frame| {
            render(frame, &mut app);
        });
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        (buf, text)
    }

    #[test]
    fn layout_80x24_no_sidebar() {
        let (_buf, text) = render_app_to_parts(80, 24);
        // At 80 columns, sidebar should NOT be visible
        assert!(
            !text.contains("Session"),
            "sidebar should not be visible at 80 cols"
        );
        // Input area should be present (the chevron ">")
        assert!(text.contains(">"), "input chevron should be visible");
    }

    #[test]
    fn layout_120x24_with_sidebar() {
        let mut app = crate::app::tests::make_test_app();
        app.sidebar_state.model_name = "gpt-4o".to_string();
        let buf = render_to_buffer(120, 24, |frame| {
            render(frame, &mut app);
        });
        let mut text = String::new();
        for y in 0..24 {
            for x in 0..120 {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("Session"),
            "sidebar 'Session' header should be visible at 120 cols"
        );
        assert!(text.contains("gpt-4o"), "sidebar should show model name");
    }

    #[test]
    fn osc7_sequence_simple_path() {
        let seq = osc7_sequence(std::path::Path::new("/Users/dev/my-project")).unwrap();
        // Should start with OSC 7 opener, contain file:// URI, end with ST
        assert!(seq.starts_with("\x1b]7;file://"));
        assert!(seq.ends_with("\x1b\\"));
        assert!(seq.contains("/Users/dev/my-project"));
    }

    #[test]
    fn osc7_sequence_percent_encodes_spaces() {
        let seq = osc7_sequence(std::path::Path::new("/Users/dev/my project")).unwrap();
        assert!(
            seq.contains("/Users/dev/my%20project"),
            "spaces must be percent-encoded, got: {seq}"
        );
    }

    #[test]
    fn osc7_sequence_percent_encodes_special_chars() {
        let seq = osc7_sequence(std::path::Path::new("/Users/dev/foo#bar")).unwrap();
        assert!(
            seq.contains("/Users/dev/foo%23bar"),
            "# must be percent-encoded, got: {seq}"
        );
    }

    #[test]
    fn layout_separator_column_has_border_bg() {
        let mut app = crate::app::tests::make_test_app();
        let buf = render_to_buffer(120, 24, |frame| {
            render(frame, &mut app);
        });
        // The separator is 1 column wide, at x = 120 - 1(sep) - 36(sidebar) = 83
        let sep_x = 83;
        let cell = &buf[(sep_x, 0)];
        assert_eq!(
            cell.bg,
            app.theme.border_color(0),
            "separator column should have theme.border_color background"
        );
    }
}
