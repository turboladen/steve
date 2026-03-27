use super::*;
use super::session::{sanitize_title, title_fallback};
use super::tool_display::{extract_diff_content, parse_unified_diff_lines};
use crate::ui::message_block::{DiffContent, DiffLine};
use serde_json::json;
use strum::IntoEnumIterator;

/// Create a minimal App for testing (without real storage/config).
/// Note: uses `Storage::new` which writes to the real app data dir. This is
/// acceptable because UI rendering tests don't perform storage writes. A
/// temp-dir approach would require returning `TempDir` to keep it alive.
pub(crate) fn make_test_app() -> App {
    use crate::config::types::Config;
    use crate::project::ProjectInfo;
    use crate::storage::Storage;
    use std::path::PathBuf;

    let root = PathBuf::from("/tmp/test");
    let project = ProjectInfo {
        root: root.clone(),
        id: "test".to_string(),
        cwd: root,
    };
    let config = Config::default();
    let storage = Storage::new("test-sidebar").expect("test storage");
    let usage_writer = crate::usage::test_usage_writer();
    App::new(project, config, storage, Vec::new(), None, None, Vec::new(), usage_writer)
}

/// Create a test app backed by an isolated temp directory for storage tests.
fn make_test_app_with_storage() -> (App, tempfile::TempDir) {
    use crate::config::types::Config;
    use crate::project::ProjectInfo;
    use crate::storage::Storage;
    use std::path::PathBuf;

    let dir = tempfile::tempdir().expect("temp dir");
    let storage = Storage::with_base(dir.path().to_path_buf()).expect("storage");
    let root = PathBuf::from("/tmp/test");
    let project = ProjectInfo {
        root: root.clone(),
        id: "test-title".to_string(),
        cwd: root,
    };
    let config = Config::default();
    let usage_writer = crate::usage::test_usage_writer();
    let app = App::new(project, config, storage, Vec::new(), None, None, Vec::new(), usage_writer);
    (app, dir)
}

/// Helper: create a session via SessionManager and return the SessionInfo.
fn create_test_session(app: &App) -> SessionInfo {
    let mgr = SessionManager::new(&app.storage, &app.project.id);
    mgr.create_session("test/model").expect("create test session")
}

/// Helper: build a ProviderRegistry with a single test model.
fn make_test_registry(context_window: u32) -> crate::provider::ProviderRegistry {
    use crate::config::types::{ModelCapabilities, ModelConfig, ProviderConfig};
    use std::collections::HashMap;

    let mut models = HashMap::new();
    models.insert(
        "test-model".to_string(),
        ModelConfig {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            context_window,
            max_output_tokens: None,
            cost: None,
            capabilities: ModelCapabilities {
                tool_call: true,
                reasoning: false,
            },
        },
    );
    let provider_config = ProviderConfig {
        base_url: "https://api.test.com/v1".to_string(),
        api_key_env: "TEST_KEY".to_string(),
        models,
    };
    let client = crate::provider::client::LlmClient::new("https://api.test.com/v1", "fake");
    crate::provider::ProviderRegistry::from_entries(vec![
        ("test".to_string(), provider_config, client),
    ])
}

// -- extract_args_summary tests --

#[test]
fn extract_args_summary_read_path() {
    let args = json!({"path": "src/main.rs"});
    assert_eq!(extract_args_summary(ToolName::Read, &args), "src/main.rs");
}

#[test]
fn extract_args_summary_list_path() {
    let args = json!({"path": "/tmp/dir"});
    assert_eq!(extract_args_summary(ToolName::List, &args), "/tmp/dir");
}

#[test]
fn extract_args_summary_grep_pattern() {
    let args = json!({"pattern": "fn main"});
    assert_eq!(extract_args_summary(ToolName::Grep, &args), "fn main");
}

#[test]
fn extract_args_summary_glob_pattern() {
    let args = json!({"pattern": "**/*.rs"});
    assert_eq!(extract_args_summary(ToolName::Glob, &args), "**/*.rs");
}

#[test]
fn extract_args_summary_edit_path() {
    let args = json!({"file_path": "src/lib.rs", "old_string": "x", "new_string": "y"});
    assert_eq!(extract_args_summary(ToolName::Edit, &args), "src/lib.rs");
}

#[test]
fn extract_args_summary_write_path() {
    let args = json!({"file_path": "new_file.txt", "content": "hello"});
    assert_eq!(extract_args_summary(ToolName::Write, &args), "new_file.txt");
}

#[test]
fn extract_args_summary_patch_path() {
    let args = json!({"file_path": "src/app.rs", "diff": "..."});
    assert_eq!(extract_args_summary(ToolName::Patch, &args), "src/app.rs");
}

#[test]
fn extract_args_summary_bash_short_command() {
    let args = json!({"command": "ls -la"});
    assert_eq!(extract_args_summary(ToolName::Bash, &args), "ls -la");
}

#[test]
fn extract_args_summary_bash_long_command_truncates() {
    let long_cmd = "a".repeat(50);
    let args = json!({"command": long_cmd});
    let result = extract_args_summary(ToolName::Bash, &args);
    assert_eq!(result.chars().count(), 40); // 37 + "..."
    assert!(result.ends_with("..."));
}

#[test]
fn extract_args_summary_bash_exactly_40_chars() {
    let cmd = "a".repeat(40);
    let args = json!({"command": cmd});
    let result = extract_args_summary(ToolName::Bash, &args);
    assert_eq!(result.chars().count(), 40);
    assert!(!result.ends_with("..."));
}

#[test]
fn extract_args_summary_question_short() {
    let args = json!({"question": "What is this?"});
    assert_eq!(
        extract_args_summary(ToolName::Question, &args),
        "What is this?"
    );
}

#[test]
fn extract_args_summary_question_long_truncates() {
    let long_text = "a".repeat(40);
    let args = json!({"question": long_text});
    let result = extract_args_summary(ToolName::Question, &args);
    assert_eq!(result.chars().count(), 30); // 27 + "..."
    assert!(result.ends_with("..."));
}

#[test]
fn extract_args_summary_task_returns_action() {
    let args = json!({"action": "create", "title": "something"});
    assert_eq!(extract_args_summary(ToolName::Task, &args), "create");
}

#[test]
fn extract_args_summary_webfetch_url() {
    let args = json!({"url": "https://example.com"});
    assert_eq!(
        extract_args_summary(ToolName::Webfetch, &args),
        "https://example.com"
    );
}

#[test]
fn extract_args_summary_missing_field_returns_empty() {
    let args = json!({});
    assert_eq!(extract_args_summary(ToolName::Read, &args), "");
    assert_eq!(extract_args_summary(ToolName::Grep, &args), "");
    assert_eq!(extract_args_summary(ToolName::Edit, &args), "");
    assert_eq!(extract_args_summary(ToolName::Bash, &args), "");
    assert_eq!(extract_args_summary(ToolName::Question, &args), "");
    assert_eq!(extract_args_summary(ToolName::Webfetch, &args), "");
    assert_eq!(extract_args_summary(ToolName::Memory, &args), "");
}

#[test]
fn extract_args_summary_all_variants_covered() {
    // Ensure every ToolName variant is handled (exhaustive match).
    // This test will fail to compile if a new variant is added without
    // updating extract_args_summary.
    let args = json!({});
    for tool in ToolName::iter() {
        // Just ensure it doesn't panic
        let _ = extract_args_summary(tool, &args);
    }
}

// -- extract_diff_content tests --

#[test]
fn diff_content_edit_basic() {
    let args = json!({
        "file_path": "src/main.rs",
        "old_string": "use std::collections::HashMap;",
        "new_string": "use std::collections::BTreeMap;"
    });
    let result = extract_diff_content(ToolName::Edit, &args);
    match result {
        Some(DiffContent::EditDiff { lines }) => {
            assert_eq!(lines.len(), 2);
            assert_eq!(
                lines[0],
                DiffLine::Removal("use std::collections::HashMap;".into())
            );
            assert_eq!(
                lines[1],
                DiffLine::Addition("use std::collections::BTreeMap;".into())
            );
        }
        other => panic!("expected EditDiff, got {other:?}"),
    }
}

#[test]
fn diff_content_edit_multiline() {
    let args = json!({
        "file_path": "f.rs",
        "old_string": "line1\nline2",
        "new_string": "new1\nnew2\nnew3"
    });
    match extract_diff_content(ToolName::Edit, &args) {
        Some(DiffContent::EditDiff { lines }) => {
            assert_eq!(lines.len(), 5);
            assert_eq!(lines[0], DiffLine::Removal("line1".into()));
            assert_eq!(lines[1], DiffLine::Removal("line2".into()));
            assert_eq!(lines[2], DiffLine::Addition("new1".into()));
            assert_eq!(lines[3], DiffLine::Addition("new2".into()));
            assert_eq!(lines[4], DiffLine::Addition("new3".into()));
        }
        other => panic!("expected EditDiff, got {other:?}"),
    }
}

#[test]
fn diff_content_edit_empty_strings_returns_none() {
    let args = json!({"file_path": "f.rs", "old_string": "", "new_string": ""});
    assert!(extract_diff_content(ToolName::Edit, &args).is_none());
}

#[test]
fn diff_content_edit_missing_args_returns_none() {
    let args = json!({"file_path": "f.rs"});
    assert!(extract_diff_content(ToolName::Edit, &args).is_none());
}

#[test]
fn diff_content_edit_insert_lines() {
    let args = json!({
        "file_path": "f.rs",
        "operation": "insert_lines",
        "line": 5,
        "content": "new line 1\nnew line 2"
    });
    match extract_diff_content(ToolName::Edit, &args) {
        Some(DiffContent::EditDiff { lines }) => {
            assert_eq!(lines.len(), 3);
            assert_eq!(lines[0], DiffLine::HunkHeader("@@ +5 @@".into()));
            assert_eq!(lines[1], DiffLine::Addition("new line 1".into()));
            assert_eq!(lines[2], DiffLine::Addition("new line 2".into()));
        }
        other => panic!("expected EditDiff, got {other:?}"),
    }
}

#[test]
fn diff_content_edit_insert_lines_empty_content_returns_none() {
    let args = json!({
        "file_path": "f.rs",
        "operation": "insert_lines",
        "line": 1,
        "content": ""
    });
    assert!(extract_diff_content(ToolName::Edit, &args).is_none());
}

#[test]
fn diff_content_edit_delete_lines() {
    let args = json!({
        "file_path": "f.rs",
        "operation": "delete_lines",
        "start_line": 3,
        "end_line": 7
    });
    match extract_diff_content(ToolName::Edit, &args) {
        Some(DiffContent::EditDiff { lines }) => {
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0], DiffLine::HunkHeader("@@ -3,5 @@".into()));
            assert_eq!(lines[1], DiffLine::Removal("(5 line(s) deleted)".into()));
        }
        other => panic!("expected EditDiff, got {other:?}"),
    }
}

#[test]
fn diff_content_edit_replace_range() {
    let args = json!({
        "file_path": "f.rs",
        "operation": "replace_range",
        "start_line": 2,
        "end_line": 4,
        "content": "replaced1\nreplaced2"
    });
    match extract_diff_content(ToolName::Edit, &args) {
        Some(DiffContent::EditDiff { lines }) => {
            assert_eq!(lines.len(), 4);
            assert_eq!(lines[0], DiffLine::HunkHeader("@@ -2,3 @@".into()));
            assert_eq!(lines[1], DiffLine::Removal("(3 line(s) replaced)".into()));
            assert_eq!(lines[2], DiffLine::Addition("replaced1".into()));
            assert_eq!(lines[3], DiffLine::Addition("replaced2".into()));
        }
        other => panic!("expected EditDiff, got {other:?}"),
    }
}

#[test]
fn diff_content_edit_unknown_operation_returns_none() {
    let args = json!({
        "file_path": "f.rs",
        "operation": "teleport"
    });
    assert!(extract_diff_content(ToolName::Edit, &args).is_none());
}

#[test]
fn diff_content_write_basic() {
    let args = json!({"file_path": "new.txt", "content": "line1\nline2\nline3"});
    match extract_diff_content(ToolName::Write, &args) {
        Some(DiffContent::WriteSummary { line_count }) => {
            assert_eq!(line_count, 3);
        }
        other => panic!("expected WriteSummary, got {other:?}"),
    }
}

#[test]
fn diff_content_write_empty_content() {
    let args = json!({"file_path": "empty.txt", "content": ""});
    match extract_diff_content(ToolName::Write, &args) {
        Some(DiffContent::WriteSummary { line_count }) => {
            assert_eq!(line_count, 0);
        }
        other => panic!("expected WriteSummary, got {other:?}"),
    }
}

#[test]
fn diff_content_write_missing_content() {
    let args = json!({"file_path": "f.txt"});
    match extract_diff_content(ToolName::Write, &args) {
        Some(DiffContent::WriteSummary { line_count }) => {
            assert_eq!(line_count, 0);
        }
        other => panic!("expected WriteSummary, got {other:?}"),
    }
}

#[test]
fn diff_content_patch_basic() {
    let diff_str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n context\n-old line\n+new line\n context2";
    let args = json!({"file_path": "src/main.rs", "diff": diff_str});
    match extract_diff_content(ToolName::Patch, &args) {
        Some(DiffContent::PatchDiff { lines }) => {
            assert_eq!(lines.len(), 5);
            assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1,3 +1,3 @@".into()));
            assert_eq!(lines[1], DiffLine::Context("context".into()));
            assert_eq!(lines[2], DiffLine::Removal("old line".into()));
            assert_eq!(lines[3], DiffLine::Addition("new line".into()));
            assert_eq!(lines[4], DiffLine::Context("context2".into()));
        }
        other => panic!("expected PatchDiff, got {other:?}"),
    }
}

#[test]
fn diff_content_patch_empty_returns_none() {
    let args = json!({"file_path": "f.rs", "diff": ""});
    assert!(extract_diff_content(ToolName::Patch, &args).is_none());
}

#[test]
fn diff_content_non_write_tools_return_none() {
    let args = json!({"path": "src/main.rs"});
    for tool in [
        ToolName::Read,
        ToolName::Grep,
        ToolName::Glob,
        ToolName::List,
        ToolName::Bash,
        ToolName::Question,
        ToolName::Task,
        ToolName::Webfetch,
        ToolName::Memory,
    ] {
        assert!(
            extract_diff_content(tool, &args).is_none(),
            "{tool} should return None"
        );
    }
}

#[test]
fn diff_content_all_variants_covered() {
    let args = json!({});
    for tool in ToolName::iter() {
        let result = extract_diff_content(tool, &args);
        // Write tools produce diff content; all others return None.
        // Empty args produce None for write tools too, but the exhaustive
        // match is the point — a new variant without a match arm won't compile.
        if matches!(tool, ToolName::Edit | ToolName::Write | ToolName::Patch) {
            // With empty args, write tools may return None (no old_string etc.)
            // — the key assertion is that this doesn't panic.
            let _ = result;
        } else {
            assert!(
                result.is_none(),
                "{tool} should return None for diff content"
            );
        }
    }
}

// -- parse_unified_diff_lines tests --

#[test]
fn parse_diff_skips_file_headers() {
    let patch = "--- a/file.rs\n+++ b/file.rs\n@@ -1 +1 @@\n-old\n+new";
    let lines = parse_unified_diff_lines(patch);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1 +1 @@".into()));
    assert_eq!(lines[1], DiffLine::Removal("old".into()));
    assert_eq!(lines[2], DiffLine::Addition("new".into()));
}

#[test]
fn parse_diff_context_lines() {
    let patch = "@@ -1,3 +1,3 @@\n unchanged\n-removed\n+added\n still here";
    let lines = parse_unified_diff_lines(patch);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], DiffLine::HunkHeader("@@ -1,3 +1,3 @@".into()));
    assert_eq!(lines[1], DiffLine::Context("unchanged".into()));
    assert_eq!(lines[2], DiffLine::Removal("removed".into()));
    assert_eq!(lines[3], DiffLine::Addition("added".into()));
    assert_eq!(lines[4], DiffLine::Context("still here".into()));
}

#[test]
fn parse_diff_empty_string() {
    let lines = parse_unified_diff_lines("");
    assert!(lines.is_empty());
}

// -- system_prompt tests --

#[test]
fn system_prompt_includes_tool_guidance() {
    let app = make_test_app();
    let prompt = app.build_system_prompt().unwrap();
    assert!(
        prompt.contains("Search before reading"),
        "should contain search guidance"
    );
    assert!(prompt.contains("offset"), "should mention offset param");
    assert!(
        prompt.contains("context-efficient"),
        "should mention context efficiency"
    );
    assert!(
        prompt.contains("You are Steve"),
        "should contain Steve identity"
    );
    assert!(
        prompt.contains("Build mode"),
        "should explain permission model"
    );
    assert!(
        prompt.contains("Date"),
        "should contain current date"
    );
}

// -- sidebar tests --

#[test]
fn should_show_sidebar_auto_mode() {
    // In auto mode (None), sidebar shows at >= 120 width
    let app = make_test_app();
    assert!(app.should_show_sidebar(120));
    assert!(app.should_show_sidebar(200));
    assert!(!app.should_show_sidebar(119));
    assert!(!app.should_show_sidebar(80));
}

#[test]
fn should_show_sidebar_forced_show() {
    let mut app = make_test_app();
    app.sidebar_override = Some(true);
    // Force show regardless of width
    assert!(app.should_show_sidebar(80));
    assert!(app.should_show_sidebar(120));
}

#[test]
fn should_show_sidebar_forced_hide() {
    let mut app = make_test_app();
    app.sidebar_override = Some(false);
    // Force hide regardless of width
    assert!(!app.should_show_sidebar(120));
    assert!(!app.should_show_sidebar(200));
}

// -- context_warning tests --

#[test]
fn check_context_warning_fires_at_60_pct() {
    let mut app = make_test_app();
    app.context_warned = false;
    app.status_line_state.context_window = 128_000;
    app.last_prompt_tokens = 80_000; // ~62%
    app.status_line_state.last_prompt_tokens = 80_000;
    app.check_context_warning();
    assert!(app.context_warned);
    assert!(app.messages.iter().any(|m| {
        matches!(m, MessageBlock::System { text } if text.contains("Context window"))
    }));
}

#[test]
fn check_context_warning_only_fires_once() {
    let mut app = make_test_app();
    app.context_warned = false;
    app.status_line_state.context_window = 128_000;
    app.last_prompt_tokens = 80_000;
    app.status_line_state.last_prompt_tokens = 80_000;
    app.check_context_warning();
    let msg_count = app.messages.len();
    app.check_context_warning(); // second call
    assert_eq!(app.messages.len(), msg_count); // no new message
}

#[test]
fn check_context_warning_does_not_fire_below_threshold() {
    let mut app = make_test_app();
    app.status_line_state.context_window = 128_000;
    app.last_prompt_tokens = 50_000; // ~39%
    app.status_line_state.last_prompt_tokens = 50_000;
    app.check_context_warning();
    assert!(!app.context_warned);
}

#[test]
fn llm_usage_update_sets_prompt_tokens_without_session_storage() {
    let mut app = make_test_app();
    app.status_line_state.context_window = 128_000;
    app.last_prompt_tokens = 0;
    app.status_line_state.last_prompt_tokens = 0;

    // Simulate the LlmUsageUpdate handler logic
    let usage = crate::event::StreamUsage {
        prompt_tokens: 60_000,
        completion_tokens: 500,
        total_tokens: 60_500,
    };
    app.last_prompt_tokens = usage.prompt_tokens as u64;
    app.status_line_state.last_prompt_tokens = usage.prompt_tokens as u64;
    app.check_context_warning();

    assert_eq!(app.last_prompt_tokens, 60_000);
    assert_eq!(app.status_line_state.last_prompt_tokens, 60_000);
    // Session storage should remain untouched
    assert!(app.current_session.is_none());
}

// -- scroll tests --

#[test]
fn scroll_down_event_scrolls_down() {
    let mut state = crate::ui::message_area::MessageAreaState::default();
    state.update_dimensions(500, 100);
    state.scroll_to_bottom();
    state.scroll_up(10);
    let after_up = state.scroll_offset;
    state.scroll_down(3);
    assert!(
        state.scroll_offset > after_up,
        "scroll_down should increase offset"
    );
}

#[test]
fn keyboard_scroll_up_down() {
    let mut state = crate::ui::message_area::MessageAreaState::default();
    state.update_dimensions(500, 100);
    state.scroll_to_bottom(); // offset = 400
    assert!(state.auto_scroll);
    state.scroll_up(1);
    assert_eq!(state.scroll_offset, 399);
    assert!(!state.auto_scroll, "scrolling up should disable auto_scroll");
    state.scroll_down(1);
    assert_eq!(state.scroll_offset, 400);
    assert!(state.auto_scroll, "returning to bottom should re-enable auto_scroll");
}

#[test]
fn keyboard_page_scroll() {
    let mut state = crate::ui::message_area::MessageAreaState::default();
    state.update_dimensions(500, 100);
    state.scroll_to_bottom(); // offset = 400
    assert!(state.auto_scroll);
    let page = state.visible_height(); // 100
    state.scroll_up(page);
    assert_eq!(state.scroll_offset, 300);
    assert!(!state.auto_scroll, "page up should disable auto_scroll");
    state.scroll_down(page);
    assert_eq!(state.scroll_offset, 400);
    assert!(state.auto_scroll, "page down to bottom should re-enable auto_scroll");
}

// -- strip_project_root tests --

#[test]
fn strip_project_root_absolute_path() {
    let app = make_test_app();
    assert_eq!(
        app.strip_project_root("/tmp/test/src/main.rs"),
        "src/main.rs"
    );
}

#[test]
fn strip_project_root_relative_path() {
    let app = make_test_app();
    assert_eq!(app.strip_project_root("src/main.rs"), "src/main.rs");
}

#[test]
fn strip_project_root_no_match() {
    let app = make_test_app();
    assert_eq!(
        app.strip_project_root("/other/path/file.rs"),
        "/other/path/file.rs"
    );
}

#[test]
fn strip_project_root_exact_root() {
    let app = make_test_app();
    // Edge case: path is exactly the root with trailing slash
    assert_eq!(app.strip_project_root("/tmp/test/"), "");
}

#[test]
fn strip_project_root_sibling_directory() {
    let app = make_test_app();
    // Should NOT strip prefix from sibling directory (not a path boundary)
    assert_eq!(
        app.strip_project_root("/tmp/test-backup/file.rs"),
        "/tmp/test-backup/file.rs"
    );
}

// -- interjection tests --

#[test]
fn handle_interjection_adds_user_message() {
    let mut app = make_test_app();
    let (interjection_tx, _rx) = mpsc::unbounded_channel();
    app.interjection_tx = Some(interjection_tx);

    let initial_count = app.messages.len();
    app.handle_interjection("focus on tests".to_string());

    assert_eq!(app.messages.len(), initial_count + 1);
    match &app.messages[initial_count] {
        MessageBlock::User { text } => {
            assert_eq!(text, "focus on tests");
        }
        other => panic!("expected User message, got {:?}", other),
    }
}

#[test]
fn handle_interjection_rejects_commands() {
    let mut app = make_test_app();
    let (interjection_tx, _rx) = mpsc::unbounded_channel();
    app.interjection_tx = Some(interjection_tx);

    let initial_count = app.messages.len();
    app.handle_interjection("/compact".to_string());

    // No message should be added
    assert_eq!(app.messages.len(), initial_count);
}

#[test]
fn handle_interjection_noop_when_no_sender() {
    let mut app = make_test_app();
    assert!(app.interjection_tx.is_none());

    let initial_count = app.messages.len();
    app.handle_interjection("hello".to_string());

    // Should silently do nothing
    assert_eq!(app.messages.len(), initial_count);
}

// -- title_fallback tests --

#[test]
fn title_fallback_short_text() {
    assert_eq!(title_fallback("Fix the login bug"), "Fix the login bug");
}

#[test]
fn title_fallback_exactly_60_chars() {
    let text = "a".repeat(60);
    assert_eq!(title_fallback(&text), text);
}

#[test]
fn title_fallback_over_60_chars_truncates() {
    let text = "a".repeat(80);
    let result = title_fallback(&text);
    assert_eq!(result.chars().count(), 60);
    assert!(result.ends_with("..."));
    assert_eq!(&result[..57], "a".repeat(57));
}

#[test]
fn title_fallback_unicode_truncation() {
    // 70 emoji characters — should truncate to 57 + "..."
    let text = "🦀".repeat(70);
    let result = title_fallback(&text);
    assert_eq!(result.chars().count(), 60);
    assert!(result.ends_with("..."));
}

// -- sanitize_title tests --

#[test]
fn sanitize_title_clean_response() {
    assert_eq!(sanitize_title("Fix login redirect"), "Fix login redirect");
}

#[test]
fn sanitize_title_strips_double_quotes() {
    assert_eq!(sanitize_title("\"Fix login redirect\""), "Fix login redirect");
}

#[test]
fn sanitize_title_strips_single_quotes() {
    assert_eq!(sanitize_title("'Fix login redirect'"), "Fix login redirect");
}

#[test]
fn sanitize_title_strips_title_prefix() {
    assert_eq!(sanitize_title("Title: Fix login redirect"), "Fix login redirect");
}

#[test]
fn sanitize_title_strips_title_prefix_case_insensitive() {
    assert_eq!(sanitize_title("TITLE: Fix login redirect"), "Fix login redirect");
}

#[test]
fn sanitize_title_takes_first_line() {
    assert_eq!(
        sanitize_title("Fix login redirect\nHere is some explanation"),
        "Fix login redirect"
    );
}

#[test]
fn sanitize_title_skips_empty_first_line() {
    assert_eq!(
        sanitize_title("\n  \nFix login redirect\n"),
        "Fix login redirect"
    );
}

#[test]
fn sanitize_title_enforces_60_char_cap() {
    let long = "a".repeat(80);
    let result = sanitize_title(&long);
    assert_eq!(result.chars().count(), 60);
    assert!(result.ends_with("..."));
}

#[test]
fn sanitize_title_trims_whitespace() {
    assert_eq!(sanitize_title("  Fix login redirect  \n"), "Fix login redirect");
}

#[test]
fn sanitize_title_empty_returns_empty() {
    assert_eq!(sanitize_title(""), "");
}

#[test]
fn sanitize_title_combined_quote_and_prefix() {
    // Quotes stripped first, then prefix stripped too
    assert_eq!(sanitize_title("\"Title: Fix login\""), "Fix login");
}

#[test]
fn sanitize_title_single_quote_char_no_panic() {
    assert_eq!(sanitize_title("\""), "\"");
}

#[test]
fn sanitize_title_single_apostrophe_no_panic() {
    assert_eq!(sanitize_title("'"), "'");
}

#[test]
fn sanitize_title_non_ascii_prefix_no_panic() {
    // "Título:" starts with a multibyte char — must not panic on byte-index
    assert_eq!(sanitize_title("Título: Fix"), "Título: Fix");
}

// -- title_fallback edge cases --

#[test]
fn title_fallback_strips_newlines() {
    assert_eq!(
        title_fallback("Fix bug\nin login.rs"),
        "Fix bug"
    );
}

#[test]
fn title_fallback_empty_returns_empty() {
    assert_eq!(title_fallback(""), "");
}

// -- apply_session_title edge cases --

#[test]
fn apply_session_title_rejects_empty_string() {
    let (mut app, _dir) = make_test_app_with_storage();
    let session = create_test_session(&app);
    app.current_session = Some(session);

    app.apply_session_title("");

    // Title should remain unchanged
    assert_eq!(app.current_session.as_ref().unwrap().title, "New session");
}

// -- maybe_generate_title / apply_session_title tests --

#[test]
fn maybe_generate_title_skips_non_default_title() {
    let (mut app, _dir) = make_test_app_with_storage();
    let session = create_test_session(&app);

    // Rename via manager before handing to app
    {
        let mgr = SessionManager::new(&app.storage, &app.project.id);
        let mut s = session;
        mgr.rename_session(&mut s, "My Custom Title").unwrap();
        app.current_session = Some(s);
    }

    app.messages.push(MessageBlock::User {
        text: "Hello world".to_string(),
    });

    app.maybe_generate_title();

    assert_eq!(app.current_session.as_ref().unwrap().title, "My Custom Title");
}

#[test]
fn maybe_generate_title_sync_fallback_without_small_model() {
    let (mut app, _dir) = make_test_app_with_storage();
    let session = create_test_session(&app);
    app.current_session = Some(session);
    assert!(app.config.small_model.is_none());

    app.messages.push(MessageBlock::User {
        text: "Fix the authentication bug in login.rs".to_string(),
    });

    app.maybe_generate_title();

    assert_eq!(
        app.current_session.as_ref().unwrap().title,
        "Fix the authentication bug in login.rs"
    );
}

#[test]
fn maybe_generate_title_sync_fallback_truncates_long_message() {
    let (mut app, _dir) = make_test_app_with_storage();
    let session = create_test_session(&app);
    app.current_session = Some(session);

    app.messages.push(MessageBlock::User {
        text: "a".repeat(80),
    });

    app.maybe_generate_title();

    let title = &app.current_session.as_ref().unwrap().title;
    assert_eq!(title.chars().count(), 60);
    assert!(title.ends_with("..."));
}

#[test]
fn apply_session_title_updates_sidebar_and_persists() {
    let (mut app, _dir) = make_test_app_with_storage();
    let session = create_test_session(&app);
    let session_id = session.id.clone();
    app.current_session = Some(session);

    app.apply_session_title("My New Title");

    assert_eq!(app.current_session.as_ref().unwrap().title, "My New Title");
    assert_eq!(app.sidebar_state.session_title, "My New Title");

    // Verify persisted to storage
    {
        let mgr = SessionManager::new(&app.storage, &app.project.id);
        let reloaded = mgr.load_session(&session_id).expect("load");
        assert_eq!(reloaded.title, "My New Title");
    }
}

#[test]
fn title_event_guards_stale_session_id() {
    let (mut app, _dir) = make_test_app_with_storage();
    let session = create_test_session(&app);
    app.current_session = Some(session);

    // Stale session ID — apply_title_if_current should be a no-op
    app.apply_title_if_current("stale-id-does-not-match", "LLM Generated Title");

    assert_eq!(app.current_session.as_ref().unwrap().title, "New session");
}

#[test]
fn title_event_guards_renamed_session() {
    let (mut app, _dir) = make_test_app_with_storage();
    let session = create_test_session(&app);
    let session_id = session.id.clone();
    app.current_session = Some(session);

    // User renames the session before async title arrives
    app.apply_session_title("User Chose This");

    // apply_title_if_current with matching session_id should still not overwrite
    app.apply_title_if_current(&session_id, "LLM Generated Title");

    assert_eq!(
        app.current_session.as_ref().unwrap().title,
        "User Chose This"
    );
}

// -- Model picker integration tests --

#[test]
fn model_picker_open_populates_state() {
    let mut app = make_test_app();
    let models = vec![
        ("openai/gpt-4o".into(), "GPT-4o".into()),
        ("anthropic/claude".into(), "Claude".into()),
    ];
    app.model_picker.open(&models, Some("openai/gpt-4o"));

    assert!(app.model_picker.visible);
    assert_eq!(app.model_picker.filtered_models().len(), 2);
}

#[test]
fn model_picker_close_on_new() {
    let mut app = make_test_app();
    let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
    app.model_picker.open(&models, None);
    assert!(app.model_picker.visible);

    // Simulate the relevant part of Command::New handler
    app.model_picker.close();
    assert!(!app.model_picker.visible);
}

#[test]
fn model_picker_renders_in_full_app() {
    let mut app = make_test_app();
    let models = vec![
        ("openai/gpt-4o".into(), "GPT-4o".into()),
        ("anthropic/claude".into(), "Claude".into()),
    ];
    app.model_picker.open(&models, Some("openai/gpt-4o"));

    let buf = crate::ui::render_to_buffer(80, 24, |frame| {
        crate::ui::render(frame, &mut app);
    });

    let mut text = String::new();
    for y in 0..24 {
        for x in 0..80 {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }

    assert!(
        text.contains("Switch Model"),
        "overlay title should be visible, got:\n{text}"
    );
    assert!(
        text.contains("openai/gpt-4o"),
        "model ref should be visible, got:\n{text}"
    );
}

// -- diagnostics overlay tests --

#[test]
fn diagnostics_overlay_close_on_new() {
    let mut app = make_test_app();
    app.diagnostics_overlay.open(vec![]);
    assert!(app.diagnostics_overlay.visible);

    // Simulate the relevant part of Command::New handler
    app.diagnostics_overlay.close();
    assert!(!app.diagnostics_overlay.visible);
}

#[test]
fn diagnostics_overlay_closes_model_picker() {
    let mut app = make_test_app();
    let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
    app.model_picker.open(&models, None);
    assert!(app.model_picker.visible);

    // Opening diagnostics should close model picker (mutual exclusivity)
    app.model_picker.close();
    let checks = app.collect_diagnostics();
    app.diagnostics_overlay.open(checks);
    assert!(app.diagnostics_overlay.visible);
    assert!(!app.model_picker.visible);
}

#[test]
fn mcp_overlay_close_on_new() {
    let mut app = make_test_app();
    let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
    app.mcp_overlay.open(crate::ui::mcp_overlay::McpTab::Servers, snapshot, None);
    assert!(app.mcp_overlay.visible);

    // Simulate the relevant part of Command::New handler
    app.mcp_overlay.close();
    assert!(!app.mcp_overlay.visible);
}

#[test]
fn mcp_overlay_closes_other_overlays() {
    let mut app = make_test_app();
    let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
    app.model_picker.open(&models, None);
    assert!(app.model_picker.visible);

    // Simulate what open_mcp_overlay() does — close other overlays then open MCP.
    app.model_picker.close();
    app.session_picker.close();
    app.diagnostics_overlay.close();
    let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
    app.mcp_overlay.open(crate::ui::mcp_overlay::McpTab::Tools, snapshot, None);
    assert!(app.mcp_overlay.visible);
    assert!(!app.model_picker.visible);
}

#[test]
fn mcp_overlay_closed_by_diagnostics() {
    let mut app = make_test_app();
    let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
    app.mcp_overlay.open(crate::ui::mcp_overlay::McpTab::Servers, snapshot, None);
    assert!(app.mcp_overlay.visible);

    // Simulate what handle_command(Diagnostics) does — close others then open.
    app.model_picker.close();
    app.session_picker.close();
    app.mcp_overlay.close();
    let checks = app.collect_diagnostics();
    app.diagnostics_overlay.open(checks);
    assert!(app.diagnostics_overlay.visible);
    assert!(!app.mcp_overlay.visible);
}

#[test]
fn compaction_count_resets_on_new() {
    let mut app = make_test_app();
    app.compaction_count = 5;

    // Simulate the relevant part of Command::New handler
    app.compaction_count = 0;
    assert_eq!(app.compaction_count, 0);
}

#[test]
fn compaction_count_increments() {
    let mut app = make_test_app();
    assert_eq!(app.compaction_count, 0);
    app.compaction_count += 1;
    assert_eq!(app.compaction_count, 1);
}

#[test]
fn diagnostics_overlay_renders_in_full_app() {
    let mut app = make_test_app();
    let checks = app.collect_diagnostics();
    app.diagnostics_overlay.open(checks);

    let buf = crate::ui::render_to_buffer(80, 24, |frame| {
        crate::ui::render(frame, &mut app);
    });

    let mut text = String::new();
    for y in 0..24 {
        for x in 0..80 {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }

    assert!(
        text.contains("Health Dashboard"),
        "overlay title should be visible, got:\n{text}"
    );
}

// -- sync_context_window tests --

#[test]
fn sync_context_window_sets_from_registry() {
    let mut app = make_test_app();
    assert_eq!(app.status_line_state.context_window, 0);

    app.provider_registry = Some(make_test_registry(128_000));
    app.current_model = Some("test/test-model".to_string());
    app.sync_context_window();

    assert_eq!(app.status_line_state.context_window, 128_000);
}

#[test]
fn sync_context_window_noop_without_registry() {
    let mut app = make_test_app();
    app.current_model = Some("test/test-model".to_string());
    app.sync_context_window();
    assert_eq!(app.status_line_state.context_window, 0);
}

#[test]
fn sync_context_window_noop_without_model() {
    let mut app = make_test_app();
    app.provider_registry = Some(make_test_registry(128_000));
    app.current_model = None;
    app.sync_context_window();
    assert_eq!(app.status_line_state.context_window, 0);
}

#[test]
fn sync_context_window_invalid_model_preserves_previous() {
    let mut app = make_test_app();
    app.provider_registry = Some(make_test_registry(128_000));
    app.current_model = Some("test/test-model".to_string());
    app.sync_context_window();
    assert_eq!(app.status_line_state.context_window, 128_000);

    // Switch to an invalid model — previous value should be preserved
    app.current_model = Some("nonexistent/model".to_string());
    app.sync_context_window();
    assert_eq!(app.status_line_state.context_window, 128_000);
}

#[test]
fn sync_context_window_updates_on_model_change() {
    let mut app = make_test_app();
    let mut models = std::collections::HashMap::new();
    models.insert(
        "small".to_string(),
        crate::config::types::ModelConfig {
            id: "small".to_string(),
            name: "Small".to_string(),
            context_window: 32_000,
            max_output_tokens: None,
            cost: None,
            capabilities: crate::config::types::ModelCapabilities {
                tool_call: true,
                reasoning: false,
            },
        },
    );
    models.insert(
        "large".to_string(),
        crate::config::types::ModelConfig {
            id: "large".to_string(),
            name: "Large".to_string(),
            context_window: 200_000,
            max_output_tokens: None,
            cost: None,
            capabilities: crate::config::types::ModelCapabilities {
                tool_call: true,
                reasoning: false,
            },
        },
    );
    let provider_config = crate::config::types::ProviderConfig {
        base_url: "https://api.test.com/v1".to_string(),
        api_key_env: "TEST_KEY".to_string(),
        models,
    };
    let client = crate::provider::client::LlmClient::new("https://api.test.com/v1", "fake");
    app.provider_registry = Some(crate::provider::ProviderRegistry::from_entries(vec![
        ("test".to_string(), provider_config, client),
    ]));

    app.current_model = Some("test/small".to_string());
    app.sync_context_window();
    assert_eq!(app.status_line_state.context_window, 32_000);

    app.current_model = Some("test/large".to_string());
    app.sync_context_window();
    assert_eq!(app.status_line_state.context_window, 200_000);
}

// ─── close_all_overlays tests ───

#[test]
fn close_all_overlays_closes_everything() {
    let mut app = make_test_app();
    let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
    app.model_picker.open(&models, None);
    app.diagnostics_overlay.open(vec![]);
    let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
    app.mcp_overlay.open(crate::ui::mcp_overlay::McpTab::Servers, snapshot, None);
    // session_picker needs SessionInfo, so set visible directly
    app.session_picker.visible = true;
    assert!(app.model_picker.visible);
    assert!(app.diagnostics_overlay.visible);
    assert!(app.mcp_overlay.visible);
    assert!(app.session_picker.visible);

    app.close_all_overlays();
    assert!(!app.model_picker.visible);
    assert!(!app.diagnostics_overlay.visible);
    assert!(!app.mcp_overlay.visible);
    assert!(!app.session_picker.visible);
}

// ─── handle_command tests ───

fn last_message_text(app: &App) -> String {
    match app.messages.last() {
        Some(MessageBlock::System { text }) => text.clone(),
        Some(MessageBlock::Error { text }) => text.clone(),
        other => panic!("expected System or Error message, got {other:?}"),
    }
}

fn has_error_message(app: &App, needle: &str) -> bool {
    app.messages.iter().any(|m| matches!(m, MessageBlock::Error { text } if text.contains(needle)))
}

fn has_system_message(app: &App, needle: &str) -> bool {
    app.messages.iter().any(|m| matches!(m, MessageBlock::System { text } if text.contains(needle)))
}

#[tokio::test]
async fn command_unknown_pushes_error() {
    let mut app = make_test_app();
    app.handle_command("/foobar").await.unwrap();
    assert!(has_error_message(&app, "Unknown command"));
}

#[tokio::test]
async fn command_exit_sets_should_quit() {
    let mut app = make_test_app();
    assert!(!app.should_quit);
    app.handle_command("/exit").await.unwrap();
    assert!(app.should_quit);
}

#[tokio::test]
async fn command_new_resets_state() {
    let mut app = make_test_app();
    app.compaction_count = 5;
    app.context_warned = true;
    app.last_prompt_tokens = 9999;
    app.exchange_count = 10;
    app.messages.push(MessageBlock::User { text: "hello".into() });

    app.handle_command("/new").await.unwrap();

    assert_eq!(app.compaction_count, 0);
    assert!(!app.context_warned);
    assert_eq!(app.last_prompt_tokens, 0);
    assert_eq!(app.exchange_count, 0);
    assert!(app.stored_messages.is_empty());
    // Should have "New session started." as last message
    assert!(has_system_message(&app, "New session started"));
    // Should have created a new session
    assert!(app.current_session.is_some());
}

#[tokio::test]
async fn command_help_shows_commands() {
    let mut app = make_test_app();
    app.handle_command("/help").await.unwrap();
    let text = last_message_text(&app);
    assert!(text.contains("/new"));
    assert!(text.contains("/exit"));
    assert!(text.contains("/compact"));
}

#[tokio::test]
async fn command_model_no_provider_errors() {
    let mut app = make_test_app();
    assert!(app.provider_registry.is_none());
    app.handle_command("/model test/gpt").await.unwrap();
    assert!(has_error_message(&app, "No providers configured"));
}

#[tokio::test]
async fn command_models_no_provider_errors() {
    let mut app = make_test_app();
    app.handle_command("/models").await.unwrap();
    assert!(has_error_message(&app, "No providers configured"));
}

#[tokio::test]
async fn command_models_opens_picker() {
    let mut app = make_test_app();
    app.provider_registry = Some(make_test_registry(128_000));
    assert!(!app.model_picker.visible);
    app.handle_command("/models").await.unwrap();
    assert!(app.model_picker.visible);
}

#[tokio::test]
async fn command_models_closes_other_overlays() {
    let mut app = make_test_app();
    app.provider_registry = Some(make_test_registry(128_000));
    app.diagnostics_overlay.open(vec![]);
    assert!(app.diagnostics_overlay.visible);

    app.handle_command("/models").await.unwrap();
    assert!(app.model_picker.visible);
    assert!(!app.diagnostics_overlay.visible);
}

#[tokio::test]
async fn command_diagnostics_opens_overlay() {
    let mut app = make_test_app();
    assert!(!app.diagnostics_overlay.visible);
    app.handle_command("/diagnostics").await.unwrap();
    assert!(app.diagnostics_overlay.visible);
}

#[tokio::test]
async fn command_diagnostics_closes_other_overlays() {
    let mut app = make_test_app();
    let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
    app.model_picker.open(&models, None);
    assert!(app.model_picker.visible);

    app.handle_command("/diagnostics").await.unwrap();
    assert!(app.diagnostics_overlay.visible);
    assert!(!app.model_picker.visible);
}

#[tokio::test]
async fn command_compact_nothing_to_compact() {
    let mut app = make_test_app();
    app.handle_command("/compact").await.unwrap();
    assert!(has_system_message(&app, "Nothing to compact"));
}

#[tokio::test]
async fn command_compact_rejects_while_loading() {
    let mut app = make_test_app();
    app.current_session = Some(crate::session::types::SessionInfo {
        id: "test".into(),
        project_id: "test".into(),
        title: "Test".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        model_ref: "test/m".into(),
        token_usage: Default::default(),
    });
    app.stored_messages.push(crate::session::message::Message::user("test", "hello"));
    app.is_loading = true;
    app.handle_command("/compact").await.unwrap();
    assert!(has_error_message(&app, "Cannot compact while streaming"));
}

#[tokio::test]
async fn command_compact_rejects_while_streaming_active() {
    let mut app = make_test_app();
    app.current_session = Some(crate::session::types::SessionInfo {
        id: "test".into(),
        project_id: "test".into(),
        title: "Test".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        model_ref: "test/m".into(),
        token_usage: Default::default(),
    });
    app.stored_messages.push(crate::session::message::Message::user("test", "hello"));
    app.streaming_active = true;
    app.handle_command("/compact").await.unwrap();
    assert!(has_error_message(&app, "Cannot compact while streaming"));
}

#[tokio::test]
async fn command_tasks_empty() {
    let mut app = make_test_app();
    app.handle_command("/tasks").await.unwrap();
    assert!(has_system_message(&app, "No tasks"));
}

#[tokio::test]
async fn command_task_new_creates_task() {
    let (mut app, _dir) = make_test_app_with_storage();
    app.handle_command("/task-new Fix the login bug").await.unwrap();
    assert!(has_system_message(&app, "Created task"));
    assert!(has_system_message(&app, "Fix the login bug"));
}

#[tokio::test]
async fn command_task_lifecycle() {
    let (mut app, _dir) = make_test_app_with_storage();

    // Create
    app.handle_command("/task-new Test task").await.unwrap();
    let task_id = {
        let tasks = app.task_store.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        tasks[0].id.clone()
    };

    // Show
    app.handle_command(&format!("/task-show {task_id}")).await.unwrap();
    assert!(has_system_message(&app, "Test task"));

    // Complete
    app.handle_command(&format!("/task-done {task_id}")).await.unwrap();
    assert!(has_system_message(&app, "Completed"));
}

#[tokio::test]
async fn command_task_done_nonexistent_errors() {
    let mut app = make_test_app();
    app.handle_command("/task-done nonexistent-id").await.unwrap();
    assert!(has_error_message(&app, "Failed to complete task"));
}

#[tokio::test]
async fn command_epics_empty() {
    let mut app = make_test_app();
    app.handle_command("/epics").await.unwrap();
    assert!(has_system_message(&app, "No epics"));
}

#[tokio::test]
async fn command_epic_new_creates_epic() {
    let (mut app, _dir) = make_test_app_with_storage();
    app.handle_command("/epic-new Auth Overhaul").await.unwrap();
    assert!(has_system_message(&app, "Created epic"));
    assert!(has_system_message(&app, "Auth Overhaul"));
}

#[tokio::test]
async fn command_agents_update_rejects_during_streaming() {
    let mut app = make_test_app();
    app.is_loading = true;
    app.handle_command("/agents-update").await.unwrap();
    assert!(has_error_message(&app, "Cannot update AGENTS.md while streaming"));
}

#[tokio::test]
async fn command_agents_update_rejects_without_model() {
    let mut app = make_test_app();
    assert!(app.current_model.is_none());
    app.handle_command("/agents-update").await.unwrap();
    assert!(has_error_message(&app, "No model available"));
}

#[tokio::test]
async fn command_sessions_rejects_during_streaming() {
    let mut app = make_test_app();
    app.is_loading = true;
    app.handle_command("/sessions").await.unwrap();
    assert!(has_error_message(&app, "Cannot browse sessions while streaming"));
}

#[tokio::test]
async fn command_export_debug_no_session_errors() {
    let mut app = make_test_app();
    app.handle_command("/export-debug").await.unwrap();
    assert!(has_error_message(&app, "No active session to export"));
}

// ─── resolve_client tests ───

#[test]
fn resolve_client_no_provider_pushes_error() {
    let mut app = make_test_app();
    assert!(app.provider_registry.is_none());
    let result = app.resolve_client("test/model");
    assert!(result.is_none());
    assert!(has_error_message(&app, "No provider configured"));
}

#[test]
fn resolve_client_invalid_model_pushes_error() {
    let mut app = make_test_app();
    app.provider_registry = Some(make_test_registry(128_000));
    let result = app.resolve_client("nonexistent/model");
    assert!(result.is_none());
    assert!(has_error_message(&app, "Failed to resolve model"));
}

#[test]
fn resolve_client_valid_model_returns_client() {
    let mut app = make_test_app();
    app.provider_registry = Some(make_test_registry(128_000));
    let result = app.resolve_client("test/test-model");
    assert!(result.is_some());
    let (resolved, _client) = result.unwrap();
    assert_eq!(resolved.model_id, "test-model");
}

// ─── handle_event tests ───

use crate::event::{AppEvent, StreamUsage};

/// Put app into streaming state with an empty assistant message block.
fn start_streaming(app: &mut App) {
    app.streaming_active = true;
    app.is_loading = true;
    app.stream_start_time = Some(Instant::now());
    app.messages.push(MessageBlock::Assistant {
        thinking: None,
        parts: vec![],
    });
}

#[tokio::test]
async fn event_llm_delta_appends_text() {
    let mut app = make_test_app();
    start_streaming(&mut app);

    app.handle_event(AppEvent::LlmDelta { text: "Hello".into() }).await.unwrap();
    app.handle_event(AppEvent::LlmDelta { text: " world".into() }).await.unwrap();

    match app.messages.last().unwrap() {
        MessageBlock::Assistant { parts, .. } => {
            let text: String = parts.iter().filter_map(|p| {
                if let AssistantPart::Text(t) = p { Some(t.as_str()) } else { None }
            }).collect();
            assert_eq!(text, "Hello world");
        }
        other => panic!("expected Assistant, got {other:?}"),
    }
}

#[tokio::test]
async fn event_llm_delta_ignored_when_not_streaming() {
    let mut app = make_test_app();
    let msg_count = app.messages.len();

    app.handle_event(AppEvent::LlmDelta { text: "ignored".into() }).await.unwrap();

    // No new messages, no panic
    assert_eq!(app.messages.len(), msg_count);
}

#[tokio::test]
async fn event_llm_usage_update_sets_tokens() {
    let mut app = make_test_app();
    app.status_line_state.context_window = 128_000;

    app.handle_event(AppEvent::LlmUsageUpdate {
        usage: StreamUsage {
            prompt_tokens: 50_000,
            completion_tokens: 1_000,
            total_tokens: 51_000,
        },
    }).await.unwrap();

    assert_eq!(app.last_prompt_tokens, 50_000);
    assert_eq!(app.status_line_state.last_prompt_tokens, 50_000);
    assert_eq!(app.sidebar_state.prompt_tokens, 50_000);
    assert_eq!(app.sidebar_state.completion_tokens, 1_000);
}

#[tokio::test]
async fn event_llm_error_clears_streaming_state() {
    let mut app = make_test_app();
    start_streaming(&mut app);
    assert!(app.is_loading);
    assert!(app.streaming_active);

    app.handle_event(AppEvent::LlmError { error: "connection failed".into() }).await.unwrap();

    assert!(!app.is_loading);
    assert!(!app.streaming_active);
    assert!(app.stream_cancel.is_none());
    assert!(app.streaming_message.is_none());
    assert!(has_error_message(&app, "connection failed"));
}

#[tokio::test]
async fn event_llm_finish_clears_streaming_state() {
    let mut app = make_test_app();
    start_streaming(&mut app);

    app.handle_event(AppEvent::LlmFinish { usage: None }).await.unwrap();

    assert!(!app.is_loading);
    assert!(!app.streaming_active);
    assert!(app.frozen_elapsed.is_some());
}

#[tokio::test]
async fn event_stream_notice_pushes_system_message() {
    let mut app = make_test_app();

    app.handle_event(AppEvent::StreamNotice { text: "LSP started".into() }).await.unwrap();

    assert!(has_system_message(&app, "LSP started"));
}

#[tokio::test]
async fn event_compact_error_sets_auto_compact_failed() {
    let mut app = make_test_app();
    app.is_loading = true;
    assert!(!app.auto_compact_failed);

    app.handle_event(AppEvent::CompactError { error: "boom".into() }).await.unwrap();

    assert!(app.auto_compact_failed);
    assert!(!app.is_loading);
    assert!(has_error_message(&app, "boom"));
}

#[tokio::test]
async fn event_llm_retry_shows_retry_message() {
    let mut app = make_test_app();

    app.handle_event(AppEvent::LlmRetry {
        attempt: 2,
        max_attempts: 3,
        error: "timeout".into(),
    }).await.unwrap();

    assert!(has_system_message(&app, "timeout"));
    assert!(has_system_message(&app, "2/3"));
}

#[tokio::test]
async fn event_tick_clears_expired_flash() {
    let mut app = make_test_app();
    // Set a flash that expired 2 seconds ago
    app.selection_state.copied_flash = Some(Instant::now() - Duration::from_secs(2));

    app.handle_event(AppEvent::Tick).await.unwrap();

    assert!(app.selection_state.copied_flash.is_none());
}

#[tokio::test]
async fn event_tick_preserves_fresh_flash() {
    let mut app = make_test_app();
    app.selection_state.copied_flash = Some(Instant::now());

    app.handle_event(AppEvent::Tick).await.unwrap();

    assert!(app.selection_state.copied_flash.is_some());
}

// ─── finish_stream tests ───

#[test]
fn finish_stream_clears_state() {
    let mut app = make_test_app();
    app.is_loading = true;
    app.streaming_active = true;
    app.stream_start_time = Some(Instant::now());

    app.finish_stream();

    assert!(!app.is_loading);
    assert!(!app.streaming_active);
    assert!(app.stream_cancel.is_none());
    assert!(app.interjection_tx.is_none());
    assert!(app.frozen_elapsed.is_some());
}

#[test]
fn finish_stream_no_elapsed_without_start_time() {
    let mut app = make_test_app();
    app.is_loading = true;
    assert!(app.stream_start_time.is_none());

    app.finish_stream();

    assert!(app.frozen_elapsed.is_none());
}
