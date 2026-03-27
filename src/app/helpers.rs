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
    ) -> Option<(crate::provider::ResolvedModel, crate::provider::client::LlmClient)> {
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
            if let Some(block) = self.messages.iter_mut().rev().find(|m| matches!(m, MessageBlock::Question { answered: None, .. })) {
                if let MessageBlock::Question { selected, free_text, .. } = block {
                    *selected = q.selected;
                    *free_text = q.free_text.clone();
                }
            }
        }
    }

    /// Mark the last unanswered Question block as answered.
    pub(super) fn mark_question_answered(&mut self, answer: &str) {
        if let Some(block) = self.messages.iter_mut().rev().find(|m| matches!(m, MessageBlock::Question { answered: None, .. })) {
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

        let Some(tx) = &self.interjection_tx else {
            return;
        };

        // Parse and resolve @ file references
        let refs = file_ref::parse_refs(&text);
        let resolved: Vec<_> = refs
            .iter()
            .filter_map(|r| file_ref::resolve_ref(r, &self.project.root))
            .collect();

        // Show errors for unresolved refs
        for r in &refs {
            if !resolved.iter().any(|rr| rr.file_ref.path == r.path) {
                self.messages.push(MessageBlock::System {
                    text: format!("Could not resolve file: {}", r.path),
                });
            }
        }

        let (display_text, api_text) = if resolved.is_empty() {
            (text.clone(), text.clone())
        } else {
            file_ref::augment_message(&text, &resolved)
        };

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
