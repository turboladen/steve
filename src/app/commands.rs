use super::*;

impl App {
    pub(super) async fn handle_command(&mut self, text: &str) -> Result<()> {
        use crate::command::Command;

        let command = match Command::parse(text) {
            Ok(cmd) => cmd,
            Err(msg) => {
                self.messages.push(MessageBlock::Error { text: msg });
                return Ok(());
            }
        };

        match command {
            Command::Exit => {
                self.should_quit = true;
            }
            Command::New => {
                // Cancel any active stream before pruning/resetting
                self.cancel_stream();
                // Prune the old session if it had no user messages
                self.prune_empty_session();
                // Create a fresh session
                self.messages.clear();
                self.stored_messages.clear();
                self.streaming_message = None;
                self.streaming_active = false;
                self.stream_start_time = None;
                self.frozen_elapsed = None;
                self.is_loading = false;
                self.exchange_count = 0;
                self.auto_compact_failed = false;
                self.context_warned = false;
                self.last_prompt_tokens = 0;
                self.current_session = None;
                self.close_all_overlays();
                // Reset tool result cache for the new session
                *self.tool_cache.lock().unwrap() = ToolResultCache::new(self.project.root.clone());
                // Clear changeset tracking, session-closed tasks, selection, and reset token counters
                // Note: tasks persist across sessions (not cleared on /new)
                // Note: mcp_servers and lsp_servers intentionally persist — they represent
                // running server processes, not per-session state.
                self.sidebar_state.changes.clear();
                self.sidebar_state.session_closed_task_ids.clear();
                self.selection_state.clear();
                self.pending_question = None;
                self.pending_agents_update = None;
                self.compaction_count = 0;
                self.autocomplete_state.hide();
                self.ensure_session();
                self.refresh_git_info();
                self.sync_sidebar_tokens();
                self.sync_diagnostics();
                self.message_area_state.scroll_to_bottom();
                self.messages.push(MessageBlock::System {
                    text: "New session started.".to_string(),
                });
                self.update_sidebar();
            }
            Command::Rename(title) => {
                if let Some(session) = &self.current_session {
                    let mgr = SessionManager::new(&self.storage, &self.project.id);
                    let mut session = session.clone();
                    if let Err(e) = mgr.rename_session(&mut session, &title) {
                        tracing::error!(error = %e, "failed to rename session");
                    }
                    self.usage_writer.update_session_title(&session.id, &title);
                    self.current_session = Some(session);
                    self.messages.push(MessageBlock::System {
                        text: format!("Session renamed to: {title}"),
                    });
                    self.update_sidebar();
                }
            }
            Command::Model(model_ref) => {
                if let Some(registry) = &self.provider_registry {
                    match registry.resolve_model(&model_ref) {
                        Ok(_) => {
                            self.current_model = Some(model_ref.to_string());
                            self.sync_context_window();
                            self.messages.push(MessageBlock::System {
                                text: format!("Switched to model: {model_ref}"),
                            });
                            self.update_sidebar();
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("{e}"),
                            });
                        }
                    }
                } else {
                    self.messages.push(MessageBlock::Error {
                        text: "No providers configured.".to_string(),
                    });
                }
            }
            Command::Models => {
                self.close_all_overlays();
                if let Some(registry) = &self.provider_registry {
                    let models = registry.list_models();
                    if models.is_empty() {
                        self.messages.push(MessageBlock::System {
                            text: "No models configured.".to_string(),
                        });
                    } else {
                        let picker_models: Vec<(String, String)> = models
                            .iter()
                            .map(|m| (m.display_ref(), m.config.name.clone()))
                            .collect();
                        let current = self.current_model.as_deref();
                        self.model_picker.open(&picker_models, current);
                    }
                } else {
                    self.messages.push(MessageBlock::Error {
                        text: "No providers configured.".to_string(),
                    });
                }
            }
            Command::Diagnostics => {
                self.close_all_overlays();
                // Run diagnostics and open the overlay
                let checks = self.collect_diagnostics();
                self.diagnostics_overlay.open(checks);
            }
            Command::Init => {
                let agents_path = self.project.cwd.join("AGENTS.md");
                if agents_path.exists() {
                    self.messages.push(MessageBlock::System {
                        text: format!("AGENTS.md already exists at {}", agents_path.display()),
                    });
                } else {
                    let default_content = "# AGENTS.md\n\nProject-specific instructions for AI coding assistants.\n\n## Guidelines\n\n- Follow existing code style and conventions.\n- Write clear, concise commit messages.\n- Add tests for new functionality.\n";
                    match std::fs::write(&agents_path, default_content) {
                        Ok(_) => {
                            let new_entry = crate::config::AgentsFile {
                                path: agents_path.clone(),
                                content: default_content.to_string(),
                            };
                            // Maintain root-first ordering: root-level inserts at front
                            if self.project.cwd == self.project.root {
                                self.agents_files.insert(0, new_entry);
                            } else {
                                self.agents_files.push(new_entry);
                            }
                            self.messages.push(MessageBlock::System {
                                text: format!("Created AGENTS.md at {}", agents_path.display()),
                            });
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Failed to create AGENTS.md: {e}"),
                            });
                        }
                    }
                }
            }
            Command::AgentsUpdate => {
                // Guard: must not already be streaming/loading
                if self.is_loading || self.streaming_active {
                    self.messages.push(MessageBlock::Error {
                        text: "Cannot update AGENTS.md while streaming.".to_string(),
                    });
                    return Ok(());
                }

                // Guard: must not already have a pending update
                if self.pending_agents_update.is_some() {
                    self.messages.push(MessageBlock::Error {
                        text: "An AGENTS.md update is already pending approval.".to_string(),
                    });
                    return Ok(());
                }

                // Use primary model (not compact/small model — this is analytical work)
                let model_ref = match &self.current_model {
                    Some(r) => r.clone(),
                    None => {
                        self.messages.push(MessageBlock::Error {
                            text: "No model available.".to_string(),
                        });
                        return Ok(());
                    }
                };

                let Some((resolved, client)) = self.resolve_client(&model_ref) else {
                    return Ok(());
                };

                // Gather project context
                let context = self.gather_project_context();

                // Show feedback
                self.messages.push(MessageBlock::System {
                    text: "Analyzing project...".to_string(),
                });
                self.message_area_state.scroll_to_bottom();
                self.is_loading = true;
                self.status_line_state
                    .set_activity(Activity::UpdatingAgents);

                let api_model_id = resolved.api_model_id().to_string();
                let event_tx = self.event_tx.clone();

                tracing::info!(
                    model = %api_model_id,
                    context_len = context.len(),
                    "starting AGENTS.md update"
                );

                // Spawn background LLM task
                tokio::spawn(async move {
                    match client
                        .simple_chat(&api_model_id, Some(AGENTS_UPDATE_SYSTEM_PROMPT), &context)
                        .await
                    {
                        Ok(proposed_content) => {
                            let _ =
                                event_tx.send(AppEvent::AgentsUpdateFinish { proposed_content });
                        }
                        Err(e) => {
                            let _ = event_tx.send(AppEvent::AgentsUpdateError {
                                error: format!("AGENTS.md update failed: {e}"),
                            });
                        }
                    }
                });
            }
            Command::Sessions => {
                if self.is_loading || self.streaming_active {
                    self.messages.push(MessageBlock::Error {
                        text: "Cannot browse sessions while streaming.".to_string(),
                    });
                    return Ok(());
                }
                self.close_all_overlays();
                let mgr = SessionManager::new(&self.storage, &self.project.id);
                match mgr.list_sessions() {
                    Ok(sessions) if sessions.is_empty() => {
                        self.messages.push(MessageBlock::System {
                            text: "No sessions found.".to_string(),
                        });
                    }
                    Ok(sessions) => {
                        let current_id = self.current_session.as_ref().map(|s| s.id.as_str());
                        self.session_picker.open(&sessions, current_id);
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to list sessions: {e}"),
                        });
                    }
                }
            }
            Command::Compact => {
                // Guard: must have a session with messages
                if self.current_session.is_none() || self.stored_messages.is_empty() {
                    self.messages.push(MessageBlock::System {
                        text: "Nothing to compact.".to_string(),
                    });
                    return Ok(());
                }

                // Guard: must not already be streaming/loading
                if self.is_loading || self.streaming_active {
                    self.messages.push(MessageBlock::Error {
                        text: "Cannot compact while streaming.".to_string(),
                    });
                    return Ok(());
                }

                // Resolve the model for summarization
                let model_ref = match self.compact_model_ref() {
                    Some(r) => r,
                    None => {
                        self.messages.push(MessageBlock::Error {
                            text: "No model available for compaction.".to_string(),
                        });
                        return Ok(());
                    }
                };

                let Some((resolved, client)) = self.resolve_client(&model_ref) else {
                    return Ok(());
                };

                // Show feedback
                let msg_count = self.stored_messages.len();
                self.messages.push(MessageBlock::System {
                    text: format!("Compacting {msg_count} messages..."),
                });
                self.message_area_state.scroll_to_bottom();
                self.is_loading = true;
                self.status_line_state.set_activity(Activity::Compacting);

                // Build the transcript to summarize
                let transcript = self.build_compact_prompt();
                let api_model_id = resolved.api_model_id().to_string();
                let event_tx = self.event_tx.clone();

                tracing::info!(
                    model = %api_model_id,
                    messages = msg_count,
                    transcript_len = transcript.len(),
                    "starting conversation compaction"
                );

                // Spawn background summarization task
                tokio::spawn(async move {
                    match client
                        .simple_chat(&api_model_id, Some(COMPACT_SYSTEM_PROMPT), &transcript)
                        .await
                    {
                        Ok(summary) => {
                            let _ = event_tx.send(AppEvent::CompactFinish { summary });
                        }
                        Err(e) => {
                            let _ = event_tx.send(AppEvent::CompactError {
                                error: format!("Compaction failed: {e}"),
                            });
                        }
                    }
                });
            }
            Command::ExportDebug => {
                let include_logs = true;
                if self.current_session.is_none() || self.stored_messages.is_empty() {
                    self.messages.push(MessageBlock::Error {
                        text: "No active session to export.".to_string(),
                    });
                } else {
                    let session = self.current_session.as_ref().unwrap();
                    let system_prompt = self.build_system_prompt();
                    let model_ref = self.current_model.as_deref();
                    let params = crate::export::ExportParams {
                        session_id: &session.id,
                        session_title: &session.title,
                        session_created_at: session.created_at,
                        token_usage: &session.token_usage,
                        messages: &self.stored_messages,
                        system_prompt,
                        model_ref,
                        project_root: &self.project.root,
                        include_logs,
                    };
                    match crate::export::export_debug(&params) {
                        Ok(path) => {
                            let display = self.strip_project_root(&path.to_string_lossy());
                            self.messages.push(MessageBlock::System {
                                text: format!("Debug export written to: {display}"),
                            });
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Export failed: {e}"),
                            });
                        }
                    }
                }
            }
            Command::Help => {
                self.messages.push(MessageBlock::System {
                    text: "Commands:\n  /new            \u{2014} Start a new session\n  /rename <t>     \u{2014} Rename current session\n  /models         \u{2014} List available models\n  /model <r>      \u{2014} Switch to a model\n  /compact        \u{2014} Compact conversation into a summary\n  /sessions       \u{2014} Browse sessions\n  /tasks          \u{2014} List all tasks\n  /task-new <t>   \u{2014} Create a task\n  /task-done <id> \u{2014} Complete a task\n  /task-show <id> \u{2014} Show task details\n  /task-edit <id> \u{2014} Edit a task (field=value)\n  /epics          \u{2014} List epics\n  /epic-new <t>   \u{2014} Create an epic\n  /export-debug   \u{2014} Export session with logs\n  /init           \u{2014} Create AGENTS.md in project root\n  /agents-update  \u{2014} Update AGENTS.md with LLM analysis\n  /help           \u{2014} Show this help\n  /exit           \u{2014} Quit\n\nKeys:\n  Enter       \u{2014} Send message\n  Shift+Enter \u{2014} Insert newline\n  Tab         \u{2014} Accept autocomplete / toggle Build\u{2013}Plan mode\n  Up/Down     \u{2014} Navigate autocomplete list\n  Ctrl+C      \u{2014} Cancel stream / quit\n  Ctrl+B      \u{2014} Toggle sidebar\n  Mouse wheel \u{2014} Scroll messages\n  Click+drag  \u{2014} Select text (auto-copies to clipboard)".to_string(),
                });
            }
            // -- Task management commands --
            Command::Tasks => {
                let tasks = self.task_store.list_tasks().unwrap_or_default();
                let epics = self.task_store.list_epics().unwrap_or_default();
                if tasks.is_empty() {
                    self.messages.push(MessageBlock::System {
                        text: "No tasks. Use /task-new <title> to create one.".to_string(),
                    });
                } else {
                    let mut output = String::new();
                    // Group tasks by epic
                    for epic in &epics {
                        let epic_tasks: Vec<_> = tasks
                            .iter()
                            .filter(|t| t.epic_id.as_deref() == Some(&epic.id))
                            .collect();
                        if !epic_tasks.is_empty() {
                            output.push_str(&format!("## {} ({})\n", epic.title, epic.id));
                            for t in &epic_tasks {
                                let marker = if t.status == crate::task::types::TaskStatus::Done {
                                    "x"
                                } else {
                                    " "
                                };
                                let bug_label = if t.kind == TaskKind::Bug {
                                    " [bug]"
                                } else {
                                    ""
                                };
                                output.push_str(&format!(
                                    "  - [{marker}] {}: {}{bug_label} [{}]\n",
                                    t.id, t.title, t.priority
                                ));
                            }
                        }
                    }
                    // Standalone tasks (no epic)
                    let standalone: Vec<_> = tasks.iter().filter(|t| t.epic_id.is_none()).collect();
                    if !standalone.is_empty() {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str("## Standalone Tasks\n");
                        for t in &standalone {
                            let marker = if t.status == crate::task::types::TaskStatus::Done {
                                "x"
                            } else {
                                " "
                            };
                            output.push_str(&format!(
                                "  - [{marker}] {}: {} [{}]\n",
                                t.id, t.title, t.priority
                            ));
                        }
                    }
                    self.messages.push(MessageBlock::System {
                        text: output.trim_end().to_string(),
                    });
                }
                self.update_sidebar();
            }
            Command::TaskNew(title) => {
                match self.task_store.create_task(
                    &title,
                    None,
                    None,
                    None,
                    Priority::default(),
                    TaskKind::Task,
                ) {
                    Ok(task) => {
                        self.messages.push(MessageBlock::System {
                            text: format!("Created task: {} \u{2014} {}", task.id, task.title),
                        });
                        self.update_sidebar();
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to create task: {e}"),
                        });
                    }
                }
            }
            Command::TaskDone(id) => match self.task_store.complete_task(&id) {
                Ok(task) => {
                    self.messages.push(MessageBlock::System {
                        text: format!("Completed: {} \u{2014} {}", task.id, task.title),
                    });
                    self.update_sidebar();
                }
                Err(e) => {
                    self.messages.push(MessageBlock::Error {
                        text: format!("Failed to complete task: {e}"),
                    });
                }
            },
            Command::TaskShow(id) => match self.task_store.get_task(&id) {
                Ok(task) => {
                    let epic_info = task
                        .epic_id
                        .as_ref()
                        .and_then(|eid| self.task_store.get_epic(eid).ok())
                        .map(|e| format!("{} ({})", e.title, e.id))
                        .unwrap_or_else(|| "(none)".to_string());
                    let text = format!(
                        "ID: {}\nType: {}\nTitle: {}\nStatus: {}\nPriority: {}\nEpic: {}\nDescription: {}\nCreated: {}",
                        task.id,
                        task.kind,
                        task.title,
                        task.status,
                        task.priority,
                        epic_info,
                        task.description.as_deref().unwrap_or("(none)"),
                        task.created_at.format("%Y-%m-%d %H:%M"),
                    );
                    self.messages.push(MessageBlock::System { text });
                }
                Err(e) => {
                    self.messages.push(MessageBlock::Error {
                        text: format!("Task not found: {e}"),
                    });
                }
            },
            Command::TaskEdit(args_str) => {
                // Parse: "<task-id> field=value field=value ..."
                let parts: Vec<&str> = args_str.splitn(2, ' ').collect();
                let id = parts[0];
                match self.task_store.get_task(id) {
                    Ok(mut task) => {
                        let mut changed = Vec::new();
                        if let Some(kv_str) = parts.get(1) {
                            for pair in kv_str.split_whitespace() {
                                if let Some((key, val)) = pair.split_once('=') {
                                    match key {
                                        "title" => {
                                            task.title = val.to_string();
                                            changed.push("title");
                                        }
                                        "priority" => match val {
                                            "high" => {
                                                task.priority = crate::task::types::Priority::High;
                                                changed.push("priority");
                                            }
                                            "medium" => {
                                                task.priority =
                                                    crate::task::types::Priority::Medium;
                                                changed.push("priority");
                                            }
                                            "low" => {
                                                task.priority = crate::task::types::Priority::Low;
                                                changed.push("priority");
                                            }
                                            _ => {
                                                self.messages.push(MessageBlock::Error {
                                                        text: format!("Invalid priority '{val}'. Use high, medium, or low."),
                                                    });
                                            }
                                        },
                                        "status" => match val {
                                            "open" => {
                                                task.status = crate::task::types::TaskStatus::Open;
                                                changed.push("status");
                                            }
                                            "in_progress" | "inprogress" => {
                                                task.status =
                                                    crate::task::types::TaskStatus::InProgress;
                                                changed.push("status");
                                            }
                                            "done" => {
                                                task.status = crate::task::types::TaskStatus::Done;
                                                changed.push("status");
                                            }
                                            _ => {
                                                self.messages.push(MessageBlock::Error {
                                                        text: format!("Invalid status '{val}'. Use open, in_progress, or done."),
                                                    });
                                            }
                                        },
                                        _ => {
                                            self.messages.push(MessageBlock::Error {
                                                text: format!("Unknown field '{key}'. Use title, priority, or status."),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        if changed.is_empty() {
                            self.messages.push(MessageBlock::Error {
                                text: "No valid fields to update. Usage: /task-edit <id> title=... priority=... status=...".to_string(),
                            });
                        } else if let Err(e) = self.task_store.update_task(&mut task) {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Failed to update task: {e}"),
                            });
                        } else {
                            self.messages.push(MessageBlock::System {
                                text: format!("Updated task {id}: changed {}.", changed.join(", ")),
                            });
                        }
                        self.update_sidebar();
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Task not found: {e}"),
                        });
                    }
                }
            }
            Command::Epics => {
                let epics = self.task_store.list_epics().unwrap_or_default();
                if epics.is_empty() {
                    self.messages.push(MessageBlock::System {
                        text: "No epics. Use /epic-new <title> to create one.".to_string(),
                    });
                } else {
                    let lines: Vec<String> = epics
                        .iter()
                        .map(|e| {
                            let ref_str = e.external_ref.as_deref().unwrap_or("");
                            let ref_part = if ref_str.is_empty() {
                                String::new()
                            } else {
                                format!(" ({ref_str})")
                            };
                            format!("  {} \u{2014} {} [{}]{ref_part}", e.id, e.title, e.status)
                        })
                        .collect();
                    self.messages.push(MessageBlock::System {
                        text: format!("## Epics\n{}", lines.join("\n")),
                    });
                }
            }
            Command::EpicNew(title) => {
                match self.task_store.create_epic(
                    &title,
                    "",
                    None,
                    crate::task::types::Priority::default(),
                ) {
                    Ok(epic) => {
                        self.messages.push(MessageBlock::System {
                            text: format!("Created epic: {} \u{2014} {}", epic.id, epic.title),
                        });
                    }
                    Err(e) => {
                        self.messages.push(MessageBlock::Error {
                            text: format!("Failed to create epic: {e}"),
                        });
                    }
                }
            }
            Command::Mcp => {
                self.open_mcp_overlay(crate::ui::mcp_overlay::McpTab::Servers, None)
                    .await;
            }
            Command::McpTools(filter) => {
                self.open_mcp_overlay(crate::ui::mcp_overlay::McpTab::Tools, filter)
                    .await;
            }
            Command::McpResources(filter) => {
                self.open_mcp_overlay(crate::ui::mcp_overlay::McpTab::Resources, filter)
                    .await;
            }
            Command::McpPrompts(filter) => {
                self.open_mcp_overlay(crate::ui::mcp_overlay::McpTab::Prompts, filter)
                    .await;
            }
        }

        Ok(())
    }

    /// Open the MCP overlay on the given tab, snapshotting current MCP state.
    async fn open_mcp_overlay(
        &mut self,
        tab: crate::ui::mcp_overlay::McpTab,
        filter: Option<String>,
    ) {
        self.close_all_overlays();

        let mgr = self.mcp_manager.lock().await;
        let snapshot = mgr.overlay_snapshot(&self.config.mcp_servers);
        drop(mgr);

        self.mcp_overlay.open(tab, snapshot, filter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{
        has_error_message, has_system_message, make_test_app, make_test_app_with_storage,
        make_test_registry,
    };

    fn last_message_text(app: &App) -> String {
        match app.messages.last() {
            Some(MessageBlock::System { text }) => text.clone(),
            Some(MessageBlock::Error { text }) => text.clone(),
            other => panic!("expected System or Error message, got {other:?}"),
        }
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
        app.messages.push(MessageBlock::User {
            text: "hello".into(),
        });

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
        app.stored_messages
            .push(crate::session::message::Message::user("test", "hello"));
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
        app.stored_messages
            .push(crate::session::message::Message::user("test", "hello"));
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
        app.handle_command("/task-new Fix the login bug")
            .await
            .unwrap();
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
        app.handle_command(&format!("/task-show {task_id}"))
            .await
            .unwrap();
        assert!(has_system_message(&app, "Test task"));

        // Complete
        app.handle_command(&format!("/task-done {task_id}"))
            .await
            .unwrap();
        assert!(has_system_message(&app, "Completed"));
    }

    #[tokio::test]
    async fn command_task_done_nonexistent_errors() {
        let mut app = make_test_app();
        app.handle_command("/task-done nonexistent-id")
            .await
            .unwrap();
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
        assert!(has_error_message(
            &app,
            "Cannot update AGENTS.md while streaming"
        ));
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
        assert!(has_error_message(
            &app,
            "Cannot browse sessions while streaming"
        ));
    }

    #[tokio::test]
    async fn command_export_debug_no_session_errors() {
        let mut app = make_test_app();
        app.handle_command("/export-debug").await.unwrap();
        assert!(has_error_message(&app, "No active session to export"));
    }
}
