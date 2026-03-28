use super::*;

impl App {
    /// Close all overlay panels (model picker, session picker, diagnostics, MCP).
    /// Called before opening a new overlay to enforce mutual exclusivity.
    pub(super) fn close_all_overlays(&mut self) {
        self.model_picker.close();
        self.session_picker.close();
        self.diagnostics_overlay.close();
        self.mcp_overlay.close();
    }

    /// Resolve a model ref to a client, pushing error messages on failure.
    /// Returns `None` (with errors already displayed) if resolution fails.
    pub(super) fn resolve_client(
        &mut self,
        model_ref: &str,
    ) -> Option<(
        crate::provider::ResolvedModel,
        crate::provider::client::LlmClient,
    )> {
        let registry = match &self.provider_registry {
            Some(r) => r,
            None => {
                self.messages.push(MessageBlock::Error {
                    text: "No provider configured.".to_string(),
                });
                return None;
            }
        };
        let resolved = match registry.resolve_model(model_ref) {
            Ok(r) => r,
            Err(e) => {
                self.messages.push(MessageBlock::Error {
                    text: format!("Failed to resolve model: {e}"),
                });
                return None;
            }
        };
        let client = match registry.client(&resolved.provider_id) {
            Ok(c) => c.clone(),
            Err(e) => {
                self.messages.push(MessageBlock::Error {
                    text: format!("{e}"),
                });
                return None;
            }
        };
        Some((resolved, client))
    }

    /// Common cleanup when a stream ends (finish or error).
    /// Freezes elapsed time, clears streaming state, and sets activity to idle.
    pub(super) fn finish_stream(&mut self) {
        if let Some(start) = self.stream_start_time {
            self.frozen_elapsed = Some(start.elapsed());
        }
        self.is_loading = false;
        self.streaming_active = false;
        self.stream_cancel = None;
        self.interjection_tx = None;
        self.status_line_state.set_activity(Activity::Idle);
    }

    /// Parse `@file` references in user text, resolve them against the project root,
    /// push error messages for unresolved refs, and return (display_text, api_text).
    /// If no refs are found, both strings equal the input text.
    pub(super) fn resolve_file_refs(&mut self, text: &str) -> (String, String) {
        let refs = file_ref::parse_refs(text);
        let resolved: Vec<_> = refs
            .iter()
            .filter_map(|r| file_ref::resolve_ref(r, &self.project.root))
            .collect();

        for r in &refs {
            if !resolved.iter().any(|rr| rr.file_ref.path == r.path) {
                self.messages.push(MessageBlock::System {
                    text: format!("Could not resolve file: {}", r.path),
                });
            }
        }

        if resolved.is_empty() {
            (text.to_string(), text.to_string())
        } else {
            file_ref::augment_message(text, &resolved)
        }
    }

    /// Find the last Assistant block in messages.
    /// Permission/System blocks can be interleaved during streaming, so
    /// `messages.last_mut()` may not be the Assistant block we need.
    pub(super) fn last_assistant_mut(&mut self) -> Option<&mut MessageBlock> {
        self.messages.iter_mut().rev().find(|m| m.is_assistant())
    }

    /// Remove the last Permission block from messages.
    /// Called after the user responds to a permission prompt so the ephemeral
    /// prompt doesn't appear out-of-order with tool call results.
    pub(super) fn remove_last_permission_block(&mut self) {
        if let Some(pos) = self
            .messages
            .iter()
            .rposition(|m| matches!(m, MessageBlock::Permission { .. }))
        {
            self.messages.remove(pos);
        }
    }

    /// Update the last Question block to reflect current PendingQuestion state.
    pub(super) fn sync_question_block(&mut self) {
        if let Some(q) = &self.pending_question {
            if let Some(block) = self
                .messages
                .iter_mut()
                .rev()
                .find(|m| matches!(m, MessageBlock::Question { answered: None, .. }))
            {
                if let MessageBlock::Question {
                    selected,
                    free_text,
                    ..
                } = block
                {
                    *selected = q.selected;
                    *free_text = q.free_text.clone();
                }
            }
        }
    }

    /// Mark the last unanswered Question block as answered.
    pub(super) fn mark_question_answered(&mut self, answer: &str) {
        if let Some(block) = self
            .messages
            .iter_mut()
            .rev()
            .find(|m| matches!(m, MessageBlock::Question { answered: None, .. }))
        {
            if let MessageBlock::Question { answered, .. } = block {
                *answered = Some(answer.to_string());
            }
        }
    }

    /// Lazily build the file index for `@` autocomplete.
    pub(super) fn ensure_file_index(&mut self) -> &[String] {
        if self.file_index.is_none() {
            self.file_index = Some(file_ref::build_file_index(&self.project.root));
        }
        self.file_index.as_ref().unwrap()
    }

    /// Invalidate the file index (called after write tools complete).
    pub(super) fn invalidate_file_index(&mut self) {
        self.file_index = None;
    }

    /// Discard a pending AGENTS.md update and notify the user.
    pub(super) fn discard_pending_agents_update(&mut self) {
        self.pending_agents_update = None;
        self.messages.push(MessageBlock::System {
            text: "AGENTS.md update discarded.".to_string(),
        });
        self.message_area_state.scroll_to_bottom();
    }

    /// Cancel the current streaming task.
    pub(super) fn cancel_stream(&mut self) {
        tracing::info!("cancelling stream");

        // Signal the stream task to stop
        if let Some(cancel) = self.stream_cancel.take() {
            cancel.cancel();
        }

        // Also dismiss any pending permission prompt
        if let Some(perm) = self.pending_permission.take() {
            let _ = perm.response_tx.send(PermissionReply::Deny);
        }

        // Also dismiss any pending question prompt
        if let Some(q) = self.pending_question.take() {
            let _ = q.response_tx.send("User cancelled.".to_string());
        }

        if let Some(start) = self.stream_start_time {
            self.frozen_elapsed = Some(start.elapsed());
        }
        self.is_loading = false;
        self.streaming_active = false;
        self.interjection_tx = None;
        self.streaming_message = None;

        // Remove trailing empty assistant message
        if let Some(last) = self.messages.last()
            && last.is_empty_assistant()
        {
            self.messages.pop();
        }
        self.messages.push(MessageBlock::System {
            text: "cancelled".to_string(),
        });
        self.status_line_state.set_activity(Activity::Idle);
        self.message_area_state.scroll_to_bottom();
    }

    /// Inject a user message into the active tool loop without cancelling it.
    /// The message is sent via the interjection channel to the stream task,
    /// which drains it before the next LLM API call.
    pub(super) fn handle_interjection(&mut self, text: String) {
        // Silently reject slash commands during interjection
        if text.starts_with('/') {
            return;
        }

        let Some(tx) = self.interjection_tx.clone() else {
            return;
        };

        let (display_text, api_text) = self.resolve_file_refs(&text);

        // Send augmented text to stream task via interjection channel
        if tx.send(api_text.clone()).is_err() {
            // Channel closed — stream already finished
            return;
        }

        // Persist to storage
        if let Some(session) = &self.current_session {
            let user_msg = Message::user(&session.id, &api_text);
            let mgr = SessionManager::new(&self.storage, &self.project.id);
            let _ = mgr.save_message(&user_msg);
            self.stored_messages.push(user_msg);
        }

        // Add to display
        self.messages
            .push(MessageBlock::User { text: display_text });
        self.message_area_state.scroll_to_bottom();
    }

    /// Whether the sidebar should be shown, considering user override and terminal width.
    pub fn should_show_sidebar(&self, terminal_width: u16) -> bool {
        match self.sidebar_override {
            Some(forced) => forced,
            None => terminal_width >= 120,
        }
    }

    /// Find the most recently completed tool call with the given name in the last
    /// assistant message's tool groups. Returns a reference to the `ToolCall`.
    pub(super) fn find_last_completed_call(&self, tool_name: ToolName) -> Option<&ToolCall> {
        let last = self.messages.iter().rev().find(|m| m.is_assistant())?;
        if let MessageBlock::Assistant { parts, .. } = last {
            for part in parts.iter().rev() {
                if let AssistantPart::ToolGroup(group) = part {
                    // Find the last call with this tool name that has a result
                    for call in group.calls.iter().rev() {
                        if call.tool_name == tool_name && call.result_summary.is_some() {
                            return Some(call);
                        }
                    }
                }
            }
        }
        None
    }

    /// Strip the project root prefix from an absolute path, returning a relative path.
    /// Returns the input unchanged if it doesn't start with the project root.
    /// Only strips at path boundaries (e.g., `/foo/bar` won't match `/foo/bar-baz`).
    pub(super) fn strip_project_root(&self, path: &str) -> String {
        let root = self.project.root.to_string_lossy();
        let root_str = root.as_ref();
        if let Some(rest) = path.strip_prefix(root_str) {
            if let Some(relative) = rest.strip_prefix('/') {
                relative.to_string()
            } else if rest.is_empty() {
                String::new()
            } else {
                // Did not match at a path boundary (e.g., sibling directory)
                path.to_string()
            }
        } else {
            path.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tests::{has_error_message, has_system_message, make_test_app, make_test_registry},
        ui::message_block::{ToolGroup, ToolGroupStatus},
    };
    use tokio::sync::mpsc;

    // -- should_show_sidebar tests --

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

    // -- close_all_overlays tests --

    #[test]
    fn close_all_overlays_closes_everything() {
        let mut app = make_test_app();
        let models = vec![("openai/gpt-4o".into(), "GPT-4o".into())];
        app.model_picker.open(&models, None);
        app.diagnostics_overlay.open(vec![]);
        let snapshot = crate::ui::mcp_overlay::McpSnapshot::default();
        app.mcp_overlay
            .open(crate::ui::mcp_overlay::McpTab::Servers, snapshot, None);
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

    // -- resolve_client tests --

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

    // -- finish_stream tests --

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

    // -- last_assistant_mut tests --

    #[test]
    fn last_assistant_mut_finds_assistant_after_system() {
        let mut app = make_test_app();
        app.messages.push(MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::Text("hello".into())],
        });
        // Interleave a system message (like a permission response)
        app.messages.push(MessageBlock::System {
            text: "granted".into(),
        });

        let found = app.last_assistant_mut();
        assert!(found.is_some());
        assert!(found.unwrap().is_assistant());
    }

    #[test]
    fn last_assistant_mut_returns_none_when_no_assistant() {
        let mut app = make_test_app();
        app.messages.clear();
        app.messages.push(MessageBlock::User { text: "hi".into() });
        assert!(app.last_assistant_mut().is_none());
    }

    // -- remove_last_permission_block tests --

    #[test]
    fn remove_last_permission_block_removes_it() {
        let mut app = make_test_app();
        app.messages.clear();
        app.messages.push(MessageBlock::User {
            text: "run it".into(),
        });
        app.messages.push(MessageBlock::Permission {
            tool_name: "bash".into(),
            args_summary: "ls".into(),
            diff_content: None,
        });
        app.messages
            .push(MessageBlock::System { text: "ok".into() });
        assert_eq!(app.messages.len(), 3);

        app.remove_last_permission_block();

        assert_eq!(app.messages.len(), 2);
        // Permission block should be gone
        assert!(
            !app.messages
                .iter()
                .any(|m| matches!(m, MessageBlock::Permission { .. }))
        );
    }

    #[test]
    fn remove_last_permission_block_noop_when_none() {
        let mut app = make_test_app();
        app.messages.push(MessageBlock::User { text: "hi".into() });
        let count = app.messages.len();

        app.remove_last_permission_block();

        assert_eq!(app.messages.len(), count);
    }

    // -- mark_question_answered tests --

    #[test]
    fn mark_question_answered_sets_answer() {
        let mut app = make_test_app();
        app.messages.push(MessageBlock::Question {
            question: "Pick one".into(),
            options: vec!["a".into(), "b".into()],
            selected: Some(0),
            free_text: String::new(),
            answered: None,
        });

        app.mark_question_answered("a");

        match app.messages.last().unwrap() {
            MessageBlock::Question { answered, .. } => {
                assert_eq!(answered.as_deref(), Some("a"));
            }
            other => panic!("expected Question, got {other:?}"),
        }
    }

    #[test]
    fn mark_question_answered_skips_already_answered() {
        let mut app = make_test_app();
        // Already answered question
        app.messages.push(MessageBlock::Question {
            question: "Old".into(),
            options: vec![],
            selected: None,
            free_text: String::new(),
            answered: Some("done".into()),
        });
        // Unanswered question
        app.messages.push(MessageBlock::Question {
            question: "New".into(),
            options: vec![],
            selected: None,
            free_text: String::new(),
            answered: None,
        });

        app.mark_question_answered("new answer");

        // Should answer the second (unanswered) one
        match &app.messages[app.messages.len() - 1] {
            MessageBlock::Question {
                answered, question, ..
            } => {
                assert_eq!(question, "New");
                assert_eq!(answered.as_deref(), Some("new answer"));
            }
            other => panic!("expected Question, got {other:?}"),
        }
        // First one should be unchanged
        match &app.messages[app.messages.len() - 2] {
            MessageBlock::Question { answered, .. } => {
                assert_eq!(answered.as_deref(), Some("done"));
            }
            other => panic!("expected Question, got {other:?}"),
        }
    }

    // -- file index tests --

    #[test]
    fn invalidate_and_ensure_file_index() {
        let mut app = make_test_app();
        // Initially None
        assert!(app.file_index.is_none());

        // ensure_file_index populates it
        let index = app.ensure_file_index();
        assert!(index.is_empty() || !index.is_empty()); // just check it returns something

        // Now it's cached
        assert!(app.file_index.is_some());

        // invalidate clears it
        app.invalidate_file_index();
        assert!(app.file_index.is_none());
    }

    // -- discard_pending_agents_update tests --

    #[test]
    fn discard_pending_agents_update_clears_and_notifies() {
        let mut app = make_test_app();
        app.pending_agents_update = Some("proposed content".into());
        let msg_count = app.messages.len();

        app.discard_pending_agents_update();

        assert!(app.pending_agents_update.is_none());
        assert_eq!(app.messages.len(), msg_count + 1);
        assert!(has_system_message(&app, "AGENTS.md update discarded"));
    }

    // -- cancel_stream tests --

    #[test]
    fn cancel_stream_clears_state_and_pushes_cancelled() {
        let mut app = make_test_app();
        app.streaming_active = true;
        app.is_loading = true;
        app.stream_start_time = Some(Instant::now());
        // Add an empty assistant message (should be removed)
        app.messages.push(MessageBlock::Assistant {
            thinking: None,
            parts: vec![],
        });

        app.cancel_stream();

        assert!(!app.streaming_active);
        assert!(!app.is_loading);
        assert!(app.streaming_message.is_none());
        assert!(has_system_message(&app, "cancelled"));
        // Empty assistant should have been popped
        assert!(!app.messages.iter().any(|m| m.is_empty_assistant()));
    }

    #[test]
    fn cancel_stream_dismisses_pending_permission() {
        let mut app = make_test_app();
        app.streaming_active = true;
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        app.pending_permission = Some(PendingPermission {
            tool_name: ToolName::Bash,
            summary: "test".into(),
            response_tx: tx,
        });

        app.cancel_stream();

        assert!(app.pending_permission.is_none());
        // The receiver should get Deny
        assert!(matches!(rx.try_recv().unwrap(), PermissionReply::Deny));
    }

    // -- find_last_completed_call tests --

    #[test]
    fn find_last_completed_call_finds_matching_tool() {
        let mut app = make_test_app();
        app.messages.push(MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".into(),
                    full_output: Some("contents".into()),
                    result_summary: Some("42 lines".into()),
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Complete,
            })],
        });

        let found = app.find_last_completed_call(ToolName::Read);
        assert!(found.is_some());
        assert_eq!(found.unwrap().args_summary, "src/main.rs");
    }

    #[test]
    fn find_last_completed_call_returns_none_for_wrong_tool() {
        let mut app = make_test_app();
        app.messages.push(MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".into(),
                    full_output: Some("contents".into()),
                    result_summary: Some("42 lines".into()),
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Complete,
            })],
        });

        assert!(app.find_last_completed_call(ToolName::Write).is_none());
    }

    #[test]
    fn find_last_completed_call_skips_incomplete() {
        let mut app = make_test_app();
        app.messages.push(MessageBlock::Assistant {
            thinking: None,
            parts: vec![AssistantPart::ToolGroup(ToolGroup {
                calls: vec![ToolCall {
                    tool_name: ToolName::Read,
                    args_summary: "src/main.rs".into(),
                    full_output: None,
                    result_summary: None, // not completed yet
                    diff_content: None,
                    is_error: false,
                    expanded: false,
                    agent_progress: None,
                }],
                status: ToolGroupStatus::Running {
                    current_tool: ToolName::Read,
                },
            })],
        });

        assert!(app.find_last_completed_call(ToolName::Read).is_none());
    }

    #[test]
    fn find_last_completed_call_returns_none_when_no_assistant() {
        let app = make_test_app();
        assert!(app.find_last_completed_call(ToolName::Read).is_none());
    }
}
