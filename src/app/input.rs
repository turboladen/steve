use super::*;

impl App {
    pub(super) async fn handle_input(&mut self, text: String) -> Result<()> {
        if text.starts_with('/') {
            return self.handle_command(&text).await;
        }

        // Ensure we have a session
        self.ensure_session();

        // Clear elapsed timer state from the previous response
        self.stream_start_time = None;
        self.frozen_elapsed = None;

        let session_id = self
            .current_session
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_default();

        // Parse and resolve @ file references
        let (display_text, api_text) = self.resolve_file_refs(&text);

        // Create and save user message
        let user_msg = Message::user(&session_id, &api_text);
        let mgr = SessionManager::new(&self.storage, &self.project.id);
        let _ = mgr.save_message(&user_msg);
        self.stored_messages.push(user_msg);

        // Add user message to display
        self.messages
            .push(MessageBlock::User { text: display_text });
        self.message_area_state.scroll_to_bottom();

        // Resolve provider + model + client
        let Some(model_ref) = &self.current_model.clone() else {
            self.messages.push(MessageBlock::Error {
                text: "No model selected. Set 'model' in .steve.jsonc or ~/.config/steve/config.jsonc.".to_string(),
            });
            return Ok(());
        };

        let Some((resolved, client)) = self.resolve_client(model_ref) else {
            return Ok(());
        };

        // Create the assistant message that will accumulate streaming deltas
        let assistant_msg = Message::assistant(&session_id, "");
        self.streaming_message = Some(assistant_msg);

        // Push an empty assistant message to the display
        self.messages.push(MessageBlock::Assistant {
            thinking: None,
            parts: vec![],
        });

        let system_prompt = self.build_system_prompt();
        self.is_loading = true;
        self.streaming_active = true;
        self.frozen_elapsed = None;
        self.stream_start_time = Some(Instant::now());
        self.status_line_state.set_activity(Activity::Thinking);
        self.status_line_state.context_window = resolved.config.context_window as u64;

        // Create a cancellation token for this stream
        let cancel_token = CancellationToken::new();
        self.stream_cancel = Some(cancel_token.clone());

        // Create interjection channel for mid-loop user messages
        let (interjection_tx, interjection_rx) = mpsc::unbounded_channel();
        self.interjection_tx = Some(interjection_tx);

        // Build conversation history from stored messages (all except the last user message,
        // which will be passed as user_message separately)
        let history = self.build_api_history();

        // Bump cache generation so mtime-less entries (grep, glob, multi-file
        // reads) from the previous turn are invalidated on next access.
        self.tool_cache
            .lock()
            .expect("lock poisoned")
            .bump_generation();

        // Launch the streaming task with tool support
        stream::spawn_stream(StreamRequest {
            stream_provider: std::sync::Arc::new(stream::OpenAIChatStream::new(
                client.inner().clone(),
            )),
            model: resolved.api_model_id().to_string(),
            system_prompt,
            history,
            user_message: api_text,
            event_tx: self.event_tx.clone(),
            tool_registry: Some(self.tool_registry.clone()),
            tool_context: Some(ToolContext {
                project_root: self.project.root.clone(),
                storage_dir: Some(self.storage.base_dir().clone()),
                task_store: Some(Arc::new(self.task_store.clone())),
                lsp_manager: Some(self.lsp_manager.clone()),
            }),
            permission_engine: Some(self.permission_engine.clone()),
            tool_cache: self.tool_cache.clone(),
            cancel_token: cancel_token.clone(),
            context_window: if self.status_line_state.context_window > 0 {
                Some(self.status_line_state.context_window)
            } else {
                None
            },
            interjection_rx,
            usage_writer: self.usage_writer.clone(),
            usage_project_id: self.project.id.clone(),
            usage_session_id: session_id.clone(),
            usage_model_cost: resolved.config.cost.clone(),
            is_plan_mode: {
                use crate::ui::input::AgentMode;
                self.input.mode == AgentMode::Plan
            },
            agent_spawner: Some(stream::AgentSpawner {
                stream_provider: std::sync::Arc::new(stream::OpenAIChatStream::new(
                    client.inner().clone(),
                )),
                primary_model: resolved.api_model_id().to_string(),
                small_model: self.config.small_model.as_ref().and_then(|model_ref| {
                    self.provider_registry
                        .as_ref()?
                        .resolve_model(model_ref)
                        .ok()
                        .map(|r| r.api_model_id().to_string())
                }),
                project_root: self.project.root.clone(),
                tool_context: ToolContext {
                    project_root: self.project.root.clone(),
                    storage_dir: Some(self.storage.base_dir().clone()),
                    task_store: Some(Arc::new(self.task_store.clone())),
                    lsp_manager: Some(self.lsp_manager.clone()),
                },
                permission_engine: Some(self.permission_engine.clone()),
                context_window: if self.status_line_state.context_window > 0 {
                    Some(self.status_line_state.context_window)
                } else {
                    None
                },
                usage_writer: self.usage_writer.clone(),
                usage_project_id: self.project.id.clone(),
                usage_session_id: session_id.clone(),
                cancel_token: cancel_token.clone(),
                mcp_manager: Some(self.mcp_manager.clone()),
            }),
            mcp_manager: Some(self.mcp_manager.clone()),
        });

        Ok(())
    }

    /// If the current session has zero user messages, delete it from storage.
    /// Called before `/new`, `switch_to_session`, and on exit to avoid
    /// accumulating empty sessions. Callers must cancel the active stream
    /// first (if any) and are responsible for clearing `self.current_session`
    /// afterward if continuing to a new session.
    pub(super) fn prune_empty_session(&self) {
        if self.exchange_count == 0
            && self.stored_messages.is_empty()
            && let Some(session) = &self.current_session
        {
            let mgr = SessionManager::new(&self.storage, &self.project.id);
            if let Err(e) = mgr.delete_session(&session.id) {
                tracing::warn!(error = %e, "failed to prune empty session");
            } else {
                tracing::info!(session_id = %session.id, "pruned empty session");
            }
        }
    }

    /// Ensure there's an active session. Creates one if needed.
    pub(super) fn ensure_session(&mut self) {
        if self.current_session.is_some() {
            return;
        }

        let model_ref = self
            .current_model
            .clone()
            .unwrap_or_else(|| "unknown/unknown".to_string());

        let mgr = SessionManager::new(&self.storage, &self.project.id);
        match mgr.create_session(&model_ref) {
            Ok(session) => {
                tracing::info!(session_id = %session.id, "new session created");
                self.usage_writer.upsert_session(SessionRecord {
                    session_id: session.id.clone(),
                    project_id: self.project.id.clone(),
                    title: session.title.clone(),
                    model_ref: session.model_ref.clone(),
                    created_at: session.created_at,
                });
                self.current_session = Some(session);
            }
            Err(_) => {
                // Silently continue without persistence if storage fails
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{
        has_error_message, has_system_message, make_test_app, make_test_app_with_storage,
        make_test_registry,
    };

    #[tokio::test]
    async fn handle_input_slash_delegates_to_command() {
        let mut app = make_test_app();
        // /help is a valid command — should not push an error
        app.handle_input("/help".into()).await.unwrap();
        assert!(has_system_message(&app, "Commands:"));
    }

    #[tokio::test]
    async fn handle_input_no_model_pushes_error() {
        let mut app = make_test_app();
        assert!(app.current_model.is_none());
        app.handle_input("hello".into()).await.unwrap();
        assert!(has_error_message(&app, "No model selected"));
    }

    #[tokio::test]
    async fn handle_input_no_provider_pushes_error() {
        let mut app = make_test_app();
        app.current_model = Some("test/model".into());
        assert!(app.provider_registry.is_none());
        app.handle_input("hello".into()).await.unwrap();
        assert!(has_error_message(&app, "No provider configured"));
    }

    #[tokio::test]
    async fn handle_input_saves_user_message() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.current_model = Some("test/test-model".into());
        app.provider_registry = Some(make_test_registry(128_000));

        let stored_before = app.stored_messages.len();
        app.handle_input("test message".into()).await.unwrap();

        // User message should be saved
        assert_eq!(app.stored_messages.len(), stored_before + 1);
        assert!(
            app.stored_messages
                .last()
                .unwrap()
                .text_content()
                .contains("test message")
        );
    }

    #[tokio::test]
    async fn handle_input_pushes_user_display_block() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.current_model = Some("test/test-model".into());
        app.provider_registry = Some(make_test_registry(128_000));

        app.handle_input("hello world".into()).await.unwrap();

        // Should have a User message block in display
        let has_user = app
            .messages
            .iter()
            .any(|m| matches!(m, MessageBlock::User { text } if text.contains("hello world")));
        assert!(has_user, "should have User message block");
    }

    #[tokio::test]
    async fn handle_input_starts_streaming() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.current_model = Some("test/test-model".into());
        app.provider_registry = Some(make_test_registry(128_000));

        assert!(!app.is_loading);
        assert!(!app.streaming_active);

        app.handle_input("go".into()).await.unwrap();

        assert!(app.is_loading);
        assert!(app.streaming_active);
        assert!(app.stream_start_time.is_some());
        assert!(app.stream_cancel.is_some());
        assert!(app.interjection_tx.is_some());
    }

    #[tokio::test]
    async fn handle_input_ensures_session() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.current_model = Some("test/test-model".into());
        app.provider_registry = Some(make_test_registry(128_000));
        app.current_session = None;

        app.handle_input("hello".into()).await.unwrap();

        assert!(app.current_session.is_some());
    }

    #[test]
    fn ensure_session_creates_when_none() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.current_session = None;

        app.ensure_session();

        assert!(app.current_session.is_some());
    }

    #[test]
    fn ensure_session_noop_when_exists() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.ensure_session(); // creates one
        let session_id = app.current_session.as_ref().unwrap().id.clone();

        app.ensure_session(); // should not create another

        assert_eq!(app.current_session.as_ref().unwrap().id, session_id);
    }

    #[test]
    fn prune_empty_session_deletes_empty() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.ensure_session();
        let session_id = app.current_session.as_ref().unwrap().id.clone();
        assert_eq!(app.exchange_count, 0);
        assert!(app.stored_messages.is_empty());

        app.prune_empty_session();

        // Session should be deleted from storage
        let mgr = SessionManager::new(&app.storage, &app.project.id);
        assert!(mgr.load_session(&session_id).is_err());
    }

    #[test]
    fn prune_empty_session_keeps_nonempty() {
        let (mut app, _dir) = make_test_app_with_storage();
        app.ensure_session();
        let session_id = app.current_session.as_ref().unwrap().id.clone();
        // Simulate that we've had exchanges
        app.exchange_count = 1;

        app.prune_empty_session();

        // Session should still exist
        let mgr = SessionManager::new(&app.storage, &app.project.id);
        assert!(mgr.load_session(&session_id).is_ok());
    }
}
