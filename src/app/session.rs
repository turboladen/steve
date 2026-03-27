use super::*;

impl App {
    /// Try to resume the last session, or silently start without one.
    pub(super) fn resume_or_new_session(&mut self) {
        let mgr = SessionManager::new(&self.storage, &self.project.id);

        if let Some(session) = mgr.last_session() {
            tracing::info!(session_id = %session.id, title = %session.title, "resuming session");

            // Load messages from storage and populate the display
            if let Ok(loaded_messages) = mgr.load_messages(&session.id) {
                self.exchange_count = loaded_messages
                    .iter()
                    .filter(|m| m.role == Role::User)
                    .count();

                for msg in &loaded_messages {
                    match msg.role {
                        Role::User => {
                            self.messages.push(MessageBlock::User {
                                text: msg.text_content(),
                            });
                        }
                        Role::Assistant => {
                            self.messages.push(MessageBlock::Assistant {
                                thinking: None,
                                parts: vec![AssistantPart::Text(msg.text_content())],
                            });
                        }
                        Role::System => continue, // Don't display system messages
                    }
                }

                self.stored_messages = loaded_messages;

                if !self.stored_messages.is_empty() {
                    self.message_area_state.scroll_to_bottom();
                }
            }

            // Restore model from session, falling back to config if the saved
            // model_ref is no longer valid (e.g. config was updated after save).
            self.current_model = Some(self.validated_model_ref(&session.model_ref));
            self.usage_writer.upsert_session(SessionRecord {
                session_id: session.id.clone(),
                project_id: self.project.id.clone(),
                title: session.title.clone(),
                model_ref: session.model_ref.clone(),
                created_at: session.created_at,
            });
            self.current_session = Some(session);
        }

        self.refresh_git_info();
        self.sync_sidebar_tokens();
        self.sync_diagnostics();
        self.update_sidebar();
    }

    /// Switch to a different session (used by session browser).
    pub(super) async fn switch_to_session(&mut self, session: SessionInfo) -> Result<()> {
        let mgr = SessionManager::new(&self.storage, &self.project.id);

        // Cancel any active stream
        if let Some(token) = self.stream_cancel.take() {
            token.cancel();
        }

        // Prune the old session if empty before switching away
        self.prune_empty_session();

        // Clear current state
        self.messages.clear();
        self.stored_messages.clear();
        self.streaming_message = None;
        self.streaming_active = false;
        self.stream_start_time = None;
        self.frozen_elapsed = None;
        self.is_loading = false;
        self.auto_compact_failed = false;
        self.context_warned = false;
        self.last_prompt_tokens = 0;
        self.exchange_count = 0;
        self.pending_permission = None;
        self.pending_question = None;
        self.pending_agents_update = None;
        self.model_picker.close();
        *self.tool_cache.lock().unwrap() = ToolResultCache::new(self.project.root.clone());

        // Load messages
        if let Ok(loaded_messages) = mgr.load_messages(&session.id) {
            self.exchange_count = loaded_messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count();
            for msg in &loaded_messages {
                match msg.role {
                    Role::User => self.messages.push(MessageBlock::User {
                        text: msg.text_content(),
                    }),
                    Role::Assistant => self.messages.push(MessageBlock::Assistant {
                        thinking: None,
                        parts: vec![AssistantPart::Text(msg.text_content())],
                    }),
                    Role::System => continue,
                }
            }
            self.stored_messages = loaded_messages;
        }

        // Update tracking
        let mut meta = mgr.load_project_meta();
        meta.last_session_id = Some(session.id.clone());
        meta.last_model = Some(session.model_ref.clone());
        let _ = mgr.save_project_meta(&meta);

        self.current_model = Some(self.validated_model_ref(&session.model_ref));
        self.sync_context_window();
        self.usage_writer.upsert_session(SessionRecord {
            session_id: session.id.clone(),
            project_id: self.project.id.clone(),
            title: session.title.clone(),
            model_ref: session.model_ref.clone(),
            created_at: session.created_at,
        });
        self.current_session = Some(session.clone());
        self.sidebar_state.changes.clear();
        self.sidebar_state.session_closed_task_ids.clear();
        self.close_all_overlays();
        self.compaction_count = 0;
        self.refresh_git_info();
        self.sync_sidebar_tokens();
        self.sync_diagnostics();
        self.messages.push(MessageBlock::System {
            text: format!("Switched to: {}", session.title),
        });
        self.message_area_state.scroll_to_bottom();
        self.update_sidebar();
        Ok(())
    }

    /// Try to auto-generate a title for the session after the first exchange.
    /// Uses `small_model` for async LLM title generation when configured,
    /// otherwise falls back to truncating the first user message.
    pub(super) fn maybe_generate_title(&mut self) {
        let Some(session) = &self.current_session else {
            return;
        };

        // Don't re-title sessions that already have a non-default title
        // (e.g., after /rename, or after compaction resets exchange_count).
        if session.title != "New session" {
            return;
        }

        let first_user_msg = self.messages.iter().find_map(|m| match m {
            MessageBlock::User { text } => Some(text.clone()),
            _ => None,
        });

        let Some(first_text) = first_user_msg else {
            return;
        };

        let fallback = title_fallback(&first_text);

        // Only use async LLM title gen when small_model is explicitly configured.
        // Unlike compact_model_ref(), we don't fall back to the main model —
        // title gen shouldn't add latency/cost to the primary model.
        let Some(model_ref) = self.config.small_model.clone() else {
            self.apply_session_title(&fallback);
            return;
        };

        let Some(registry) = &self.provider_registry else {
            self.apply_session_title(&fallback);
            return;
        };

        let resolved = match registry.resolve_model(&model_ref) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "failed to resolve title model, using fallback");
                self.apply_session_title(&fallback);
                return;
            }
        };

        let client = match registry.client(&resolved.provider_id) {
            Ok(c) => c.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to get title model client, using fallback");
                self.apply_session_title(&fallback);
                return;
            }
        };

        let session_id = session.id.clone();
        let api_model_id = resolved.api_model_id().to_string();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            match client
                .simple_chat(&api_model_id, Some(TITLE_SYSTEM_PROMPT), &first_text)
                .await
            {
                Ok(raw) => {
                    let title = sanitize_title(&raw);
                    if title.is_empty() {
                        let _ = event_tx.send(AppEvent::TitleError {
                            session_id,
                            fallback_title: fallback,
                        });
                    } else {
                        let _ = event_tx.send(AppEvent::TitleGenerated { session_id, title });
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "LLM title generation failed, using fallback");
                    let _ = event_tx.send(AppEvent::TitleError {
                        session_id,
                        fallback_title: fallback,
                    });
                }
            }
        });
    }

    /// Apply a title only if `session_id` matches the current session and
    /// the title is still the default "New session" (guards against stale
    /// events after /new or /rename).
    pub(super) fn apply_title_if_current(&mut self, session_id: &str, title: &str) {
        let should_apply = self
            .current_session
            .as_ref()
            .is_some_and(|s| s.id == session_id && s.title == "New session");
        if should_apply {
            self.apply_session_title(title);
        }
    }

    /// Apply a generated title to the current session, persisting to storage.
    pub(super) fn apply_session_title(&mut self, title: &str) {
        if title.is_empty() {
            return;
        }
        let Some(session) = &self.current_session else {
            return;
        };
        let mgr = SessionManager::new(&self.storage, &self.project.id);
        let mut session = session.clone();
        if let Err(e) = mgr.rename_session(&mut session, title) {
            tracing::error!(error = %e, "failed to rename session");
        }
        self.usage_writer.update_session_title(&session.id, title);
        self.current_session = Some(session);
        self.update_sidebar();
    }
}

/// Enforce a 60-char cap on a title, appending "..." if truncated.
pub(super) fn truncate_title(text: &str) -> String {
    if text.chars().count() > 60 {
        let truncated: String = text.chars().take(57).collect();
        format!("{truncated}...")
    } else {
        text.to_string()
    }
}

/// Truncate the first user message to produce a sync fallback session title.
/// Takes only the first non-empty line (handles Shift+Enter newlines).
pub(super) fn title_fallback(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(text)
        .trim();
    truncate_title(line)
}

/// Clean up an LLM-generated title: trim whitespace, strip surrounding quotes,
/// remove common preamble prefixes, and enforce a 60-char cap.
pub(super) fn sanitize_title(raw: &str) -> String {
    // Take only the first non-empty line (LLMs sometimes add explanation after).
    let line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(raw)
        .trim();

    // Strip one layer of matching surrounding quotes (must be at least 2 chars).
    let stripped = if line.len() >= 2
        && ((line.starts_with('"') && line.ends_with('"'))
            || (line.starts_with('\'') && line.ends_with('\'')))
    {
        &line[1..line.len() - 1]
    } else {
        line
    };

    // Strip common preamble prefix (case-insensitive, ASCII-safe guard).
    let cleaned = if stripped.len() >= 6
        && stripped.is_char_boundary(6)
        && stripped[..6].eq_ignore_ascii_case("title:")
    {
        stripped[6..].trim()
    } else {
        stripped
    };

    truncate_title(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{make_test_app_with_storage, create_test_session};

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
}
