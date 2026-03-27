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
