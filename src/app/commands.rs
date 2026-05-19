use std::path::PathBuf;

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
                *self.tool_cache.lock().expect("lock poisoned") =
                    ToolResultCache::new(self.project.root.clone());
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
            Command::LspDiagnostics => {
                self.close_all_overlays();
                let snapshot = self.collect_lsp_diagnostics_snapshot();
                self.lsp_diagnostics_overlay.open(snapshot);
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
            Command::ExportScenario => {
                self.handle_export_scenario().await;
            }
            Command::ExportDebug => {
                let include_logs = true;
                if self.stored_messages.is_empty() {
                    self.messages.push(MessageBlock::Error {
                        text: "No active session to export.".to_string(),
                    });
                } else if let Some(session) = self.current_session.as_ref() {
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
                    text: "Commands:\n  /new             \u{2014} Start a new session\n  /rename <t>      \u{2014} Rename current session\n  /models          \u{2014} List available models\n  /model <r>       \u{2014} Switch to a model\n  /compact         \u{2014} Compact conversation into a summary\n  /sessions        \u{2014} Browse sessions\n  /tasks           \u{2014} List all tasks\n  /task-new <t>    \u{2014} Create a task\n  /task-done <id>  \u{2014} Complete a task\n  /task-show <id>  \u{2014} Show task details\n  /task-edit <id>  \u{2014} Edit a task (field=value)\n  /epics           \u{2014} List epics\n  /epic-new <t>    \u{2014} Create an epic\n  /export-debug    \u{2014} Export session with logs\n  /export-scenario \u{2014} Export session as eval scenario scaffold\n  /init            \u{2014} Create AGENTS.md in project root\n  /agents-update   \u{2014} Update AGENTS.md with LLM analysis\n  /help            \u{2014} Show this help\n  /exit            \u{2014} Quit\n\nKeys:\n  Enter       \u{2014} Send message\n  Shift+Enter \u{2014} Insert newline\n  Tab         \u{2014} Accept autocomplete / toggle Build\u{2013}Plan mode\n  Up/Down     \u{2014} Navigate autocomplete list\n  Ctrl+C      \u{2014} Cancel stream / quit\n  Ctrl+B      \u{2014} Toggle sidebar\n  Mouse wheel \u{2014} Scroll messages\n  Click+drag  \u{2014} Select text (auto-copies to clipboard)".to_string(),
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
                                let marker = if t.status == crate::task::TaskStatus::Done {
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
                            let marker = if t.status == crate::task::TaskStatus::Done {
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
                        task.created_at.display_short(),
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
                                                task.priority = crate::task::Priority::High;
                                                changed.push("priority");
                                            }
                                            "medium" => {
                                                task.priority = crate::task::Priority::Medium;
                                                changed.push("priority");
                                            }
                                            "low" => {
                                                task.priority = crate::task::Priority::Low;
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
                                                task.status = crate::task::TaskStatus::Open;
                                                changed.push("status");
                                            }
                                            "in_progress" | "inprogress" => {
                                                task.status = crate::task::TaskStatus::InProgress;
                                                changed.push("status");
                                            }
                                            "done" => {
                                                task.status = crate::task::TaskStatus::Done;
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
                    crate::task::Priority::default(),
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

    /// `/export-scenario` handler. Prompts the user for a scenario name via
    /// the same `AppEvent::QuestionRequest` mechanism the LLM `question` tool
    /// uses, then writes a `scenario.toml` scaffold to Steve's data dir at
    /// `<data-dir>/scenarios/<name>/`. Output is intentionally NOT under
    /// `project.root` — Steve usually runs in some user project that is not
    /// Steve's own source repo. The user `cp -r`'s the resulting directory
    /// into Steve's `eval/scenarios/` when ready.
    ///
    /// The prompt/await dance MUST run on a spawned task, not inline:
    /// `handle_command` is reached from `handle_event` while it holds an
    /// exclusive `&mut self`, and the user's reply is sent from inside a
    /// later `handle_event` call. Inline awaiting would deadlock the event
    /// loop.
    async fn handle_export_scenario(&mut self) {
        // Extract the scaffold inputs from BOTH data sources while we still
        // hold `&mut self`. Tool calls live only in the UI `MessageBlock`
        // layer (`AppEvent::LlmToolCall` updates `self.messages` but never
        // pushes `MessagePart::ToolCall` to `streaming_message`), so the
        // spawned task can't reach them after we let go of the borrow.
        let user_turns = match collect_user_turns(&self.stored_messages) {
            Ok(turns) => turns,
            Err(UserTurnError::MidStreamInterjection) => {
                self.messages.push(MessageBlock::Error {
                    text: "Session contains a mid-stream interjection \
                     (consecutive user messages with no assistant reply between them). \
                     The eval runner sends each user turn only after the previous \
                     assistant response goes idle, so a scenario generated from this \
                     session wouldn't replay with the original timing or LLM context. \
                     Export a session without interjections."
                        .to_string(),
                });
                return;
            }
        };

        // Guard against `stored_messages` containing only assistant text
        // (e.g. after `/compact`, which replaces the whole vector with a
        // single assistant summary at `event_loop.rs:637`). The previous
        // `stored_messages.is_empty()` check passed in that case and the
        // user only saw the failure after typing a scenario name and
        // having the spawned task bail on empty `user_turns` — fail loud
        // up front instead.
        if user_turns.is_empty() {
            let msg = if self.stored_messages.is_empty() {
                "No active session to export.".to_string()
            } else {
                "Session has no user turns to export (was it just compacted?).".to_string()
            };
            self.messages.push(MessageBlock::Error { text: msg });
            return;
        }

        // Resolve the data-dir base path eagerly. The spawned task takes
        // an already-resolved `PathBuf` so tests can drive it against a
        // tempdir instead of writing to the user's real data directory.
        let data_dir = match directories::ProjectDirs::from("", "", "steve") {
            Some(dirs) => dirs.data_dir().to_path_buf(),
            None => {
                self.messages.push(MessageBlock::Error {
                    text: "Could not resolve Steve's data directory.".to_string(),
                });
                return;
            }
        };

        let fixture_paths = collect_fixture_paths(&self.messages);
        let session_trace = build_session_trace(&self.messages);
        let event_tx = self.event_tx.clone();
        tokio::spawn(export_scenario_task(
            data_dir,
            user_turns,
            fixture_paths,
            session_trace,
            event_tx,
        ));
    }
}

#[derive(Debug, PartialEq, Eq)]
enum UserTurnError {
    /// Two consecutive `Role::User` messages with no assistant turn
    /// between them — a mid-stream interjection (see
    /// `app/helpers.rs::handle_interjection`). The eval runner sends
    /// each `user_turn` only after the previous assistant response has
    /// gone idle (`eval/runner.rs:166`), so a scenario generated from
    /// such a session wouldn't replay with the same timing or LLM
    /// context. Refuse rather than silently produce a misleading
    /// scaffold.
    MidStreamInterjection,
}

/// Pull non-empty, trimmed user-turn text from persisted messages.
/// System messages don't break the User/Assistant alternation check —
/// they're framework-injected, not part of the conversation order.
fn collect_user_turns(messages: &[Message]) -> Result<Vec<String>, UserTurnError> {
    let mut turns = Vec::new();
    let mut prev_role: Option<Role> = None;
    for m in messages {
        match m.role {
            Role::User => {
                if matches!(prev_role, Some(Role::User)) {
                    return Err(UserTurnError::MidStreamInterjection);
                }
                let text = m.text_content();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    turns.push(trimmed.to_string());
                }
                prev_role = Some(Role::User);
            }
            Role::Assistant => prev_role = Some(Role::Assistant),
            Role::System => {}
        }
    }
    Ok(turns)
}

/// Walk UI `MessageBlock`s and pull workspace-relative paths the agent
/// touched. Only completed, non-erroring `Read`/`Edit`/`Write`/`Patch`
/// calls count — the same `Completed`-state semantics the previous
/// `Message`-walking helper used, applied at the UI layer where tool
/// calls actually live. Paths are validated through
/// `scaffold::is_valid_fixture_candidate` so the emitted suggestion
/// block never points the user at a path `Scenario::validate` would
/// later reject.
/// Render the captured session as a `SESSION_TRACE.md`-shaped markdown
/// string. Goes into a sidecar file alongside `scenario.toml` — gives the
/// scenario author the original tool-call sequence and final assistant
/// message to mine for concrete `pass_when` / `fail_when` text and
/// `final_message_contains` sentinels. Not loaded by the eval runner
/// (it's not in `copy_fixtures`), so the author can keep or delete it.
///
/// Walks UI `MessageBlock`s in order, grouping each User entry with the
/// Assistant block(s) that follow into a numbered turn. System / Error /
/// Permission / Question blocks are skipped — they're framework chatter
/// rather than session content.
fn build_session_trace(blocks: &[MessageBlock]) -> String {
    use crate::ui::message_block::AssistantPart;
    let mut out = String::new();
    out.push_str("# Session trace\n\n");
    out.push_str(
        "This is what happened in the original session that generated the\n\
         neighboring `scenario.toml`. Use it as a reference while filling\n\
         in the TODOs — the tool-call sequence and final assistant message\n\
         below are the raw material for concrete `pass_when` / `fail_when`\n\
         text and `final_message_contains` sentinels.\n\
         \n\
         This file is **not** loaded by the eval runner (it's not listed in\n\
         `[setup].copy_fixtures`). Keep it committed as documentation of\n\
         why the scenario exists, or delete it before committing if you\n\
         prefer scenario directories to stay minimal.\n\n",
    );

    let mut turn_idx = 0;
    let mut turn_open = false;

    for block in blocks {
        match block {
            MessageBlock::User { text } => {
                turn_idx += 1;
                turn_open = true;
                out.push_str(&format!("## Turn {turn_idx}\n\n"));
                out.push_str("**User:**\n\n");
                emit_quoted(&mut out, text);
                out.push('\n');
            }
            MessageBlock::Assistant { parts, .. } if turn_open => {
                let mut tool_calls: Vec<&crate::ui::message_block::ToolCall> = Vec::new();
                let mut text_chunks: Vec<&str> = Vec::new();
                for part in parts {
                    match part {
                        AssistantPart::Text(t) => text_chunks.push(t),
                        AssistantPart::ToolGroup(g) => {
                            for call in &g.calls {
                                tool_calls.push(call);
                            }
                        }
                    }
                }

                if !tool_calls.is_empty() {
                    out.push_str("**Tool calls:**\n\n");
                    for (idx, call) in tool_calls.iter().enumerate() {
                        let err = if call.is_error { " [ERROR]" } else { "" };
                        let summary = if call.args_summary.is_empty() {
                            String::new()
                        } else {
                            format!(" `{}`", call.args_summary)
                        };
                        out.push_str(&format!(
                            "{}. `{}`{summary}{err}\n",
                            idx + 1,
                            call.tool_name,
                        ));
                        if let Some(output) = &call.full_output
                            && !output.trim().is_empty()
                        {
                            out.push_str("\n   ```\n");
                            for line in truncate_trace_output(output, 20).lines() {
                                if line.is_empty() {
                                    out.push('\n');
                                } else {
                                    out.push_str("   ");
                                    out.push_str(line);
                                    out.push('\n');
                                }
                            }
                            out.push_str("   ```\n\n");
                        }
                    }
                }

                let assistant_text = text_chunks.concat();
                if !assistant_text.trim().is_empty() {
                    out.push_str("**Assistant:**\n\n");
                    emit_quoted(&mut out, &assistant_text);
                    out.push('\n');
                }
            }
            _ => {}
        }
    }

    out
}

fn emit_quoted(out: &mut String, text: &str) {
    for line in text.lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn truncate_trace_output(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        return s.to_string();
    }
    let head = lines[..max_lines].join("\n");
    let omitted = lines.len() - max_lines;
    format!("{head}\n... ({omitted} more lines)")
}

fn collect_fixture_paths(blocks: &[MessageBlock]) -> Vec<PathBuf> {
    use crate::ui::message_block::AssistantPart;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for block in blocks {
        let MessageBlock::Assistant { parts, .. } = block else {
            continue;
        };
        for part in parts {
            let AssistantPart::ToolGroup(group) = part else {
                continue;
            };
            for call in &group.calls {
                if call.is_error || call.full_output.is_none() {
                    // is_error: agent attempted but failed
                    // full_output none: call hasn't returned yet
                    continue;
                }
                if !is_fixture_candidate_tool(call.tool_name) {
                    continue;
                }
                let Some(path) = path_from_args_summary(call.tool_name, &call.args_summary) else {
                    continue;
                };
                if !crate::eval::scaffold::is_valid_fixture_candidate(&path) {
                    continue;
                }
                let key = path.to_string_lossy().into_owned();
                if seen.insert(key) {
                    out.push(path);
                }
            }
        }
    }
    out
}

fn is_fixture_candidate_tool(tool: ToolName) -> bool {
    matches!(
        tool,
        ToolName::Read | ToolName::Edit | ToolName::Write | ToolName::Patch
    )
}

/// Recover a path from a `ToolCall.args_summary` string. Summaries come
/// from `extract_args_summary` (`app/tool_display.rs:11`) and are
/// display-shaped — for path-bearing tools the path IS the summary in
/// the simple case. Read has three special shapes the display layer
/// adds, all stripped here by *exact* trailing-suffix match (NOT
/// "first parenthesized substring") so a real filename like
/// `docs/foo (draft).md` survives intact:
///
/// - `"{path} (count)"` — count-only mode
/// - `"{path} (tail N)"` — last-N-lines mode
/// - `"{N} files"` — multi-path `paths` array; no recoverable single path
fn path_from_args_summary(tool: ToolName, summary: &str) -> Option<PathBuf> {
    if summary.is_empty() {
        return None;
    }
    match tool {
        ToolName::Edit | ToolName::Write | ToolName::Patch => Some(PathBuf::from(summary)),
        ToolName::Read => {
            if is_multi_path_read_summary(summary) {
                return None;
            }
            if let Some(prefix) = summary.strip_suffix(" (count)") {
                return if prefix.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(prefix))
                };
            }
            if let Some(prefix) = strip_tail_suffix(summary) {
                return Some(PathBuf::from(prefix));
            }
            Some(PathBuf::from(summary))
        }
        _ => None,
    }
}

/// True for the `extract_args_summary` shape `"{N} files"` where N is one
/// or more digits — a multi-path Read whose individual paths aren't
/// recoverable from the display string.
fn is_multi_path_read_summary(summary: &str) -> bool {
    let Some(prefix) = summary.strip_suffix(" files") else {
        return false;
    };
    !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit())
}

/// Strip the exact `" (tail N)"` suffix where N is one or more digits.
/// Returns the path prefix when matched, `None` otherwise. Does NOT match
/// arbitrary parenthesized content — `"docs/foo (draft).md"` falls through.
fn strip_tail_suffix(summary: &str) -> Option<&str> {
    let after_paren = summary.strip_suffix(')')?;
    let (prefix, n) = after_paren.rsplit_once(" (tail ")?;
    if prefix.is_empty() || n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(prefix)
}

/// Background task driving the `/export-scenario` flow. Runs off the event
/// loop so the await on the user's name reply doesn't block dispatch. All
/// UI feedback flows back via `AppEvent::ExportScenarioFinish` /
/// `ExportScenarioError`.
///
/// `data_dir` is the base directory under which `scenarios/<name>/scenario.toml`
/// is written. Production code resolves it via `directories::ProjectDirs`
/// in `handle_export_scenario`; tests pass a tempdir to exercise the
/// scaffold-write branch hermetically.
async fn export_scenario_task(
    data_dir: PathBuf,
    user_turns: Vec<String>,
    fixture_paths: Vec<PathBuf>,
    session_trace: String,
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    let send_err = |error: String| {
        let _ = event_tx.send(AppEvent::ExportScenarioError { error });
    };

    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let req = crate::event::QuestionRequest {
        call_id: "slash-export-scenario".to_string(),
        question: "Scenario name (lowercase letters, digits, hyphens; leading _ ok):".to_string(),
        options: Vec::new(),
        response_tx,
    };
    if event_tx.send(AppEvent::QuestionRequest(req)).is_err() {
        return; // event loop is gone; nothing to surface
    }

    let raw_name = match response_rx.await {
        Ok(name) => name,
        Err(_) => {
            // The tx half was dropped without ever being sent. The only
            // path that drops a `pending_question` without sending is
            // `Command::New`'s reset (`commands.rs:50`), so distinguish
            // this from an explicit cancel to keep the diagnosis clear.
            send_err("Scenario export aborted (session reset).".to_string());
            return;
        }
    };
    let name = raw_name.trim();
    // Key handlers send these sentinel strings on Esc / Ctrl+C / explicit
    // "User declined" — treat both as cancellation rather than slug attempts.
    if name.is_empty() || name == "User cancelled." || name == "User declined to answer." {
        send_err("Scenario export cancelled.".to_string());
        return;
    }
    if !is_valid_scenario_name(name) {
        send_err(format!(
            "Invalid scenario name {name:?}. \
             Use lowercase letters, digits, hyphens \
             (a leading underscore is also allowed, matching the convention \
             used by `_smoke` and similar scenarios)."
        ));
        return;
    }

    let dir = data_dir.join("scenarios").join(name);
    let path = dir.join("scenario.toml");

    let toml = match crate::eval::scaffold::build_scaffold(crate::eval::scaffold::ScaffoldInput {
        name,
        user_turns,
        fixture_paths,
    }) {
        Ok(s) => s,
        Err(e) => {
            send_err(format!("Failed to build scaffold: {e:#}"));
            return;
        }
    };

    // If scenario.toml already exists, prompt the user for overwrite
    // confirmation rather than refusing outright. "Cancel" defaults
    // first so a casual Enter doesn't blow away the existing manifest.
    // The corresponding SESSION_TRACE.md gets the same treatment —
    // they're written as a pair and treated as a pair when overwriting.
    let scenario_existed_before = path.exists();
    let overwrite_mode = if scenario_existed_before {
        let (otx, orx) = tokio::sync::oneshot::channel();
        let req = crate::event::QuestionRequest {
            call_id: "slash-export-scenario-overwrite".to_string(),
            question: format!(
                "scenario.toml already exists at {}. Overwrite it (and SESSION_TRACE.md)? \
                 Other files in the directory (e.g. fixtures you've already copied in) \
                 are preserved.",
                path.display()
            ),
            options: vec!["Cancel".to_string(), "Overwrite".to_string()],
            response_tx: otx,
        };
        if event_tx.send(AppEvent::QuestionRequest(req)).is_err() {
            return;
        }
        let answer = match orx.await {
            Ok(a) => a,
            Err(_) => {
                send_err("Scenario export aborted (session reset).".to_string());
                return;
            }
        };
        if answer == "Overwrite" {
            true
        } else {
            send_err("Scenario export cancelled (existing scenario preserved).".to_string());
            return;
        }
    } else {
        false
    };

    // Track whether `dir` already existed BEFORE create_dir_all so the
    // failure-cleanup branch only removes directories this task created.
    // Without this, a user with a pre-existing empty `<name>/` directory
    // would lose it if our write step happened to fail.
    let dir_existed_before = dir.exists();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        send_err(format!("Failed to create {}: {e}", dir.display()));
        return;
    }

    // Open mode depends on overwrite_mode: when the user explicitly
    // confirmed overwrite we use create+truncate (which replaces the
    // file). Otherwise we use create_new, which atomically refuses to
    // overwrite — guarding against a TOCTOU race where the file
    // materializes between our existence check and the open.
    //
    // Cleanup-on-failure only removes the file if we created it.
    // Overwrite mode wrote into the user's existing file, so removing
    // it on failure would just compound the data loss (we can't
    // un-overwrite anyway).
    let cleanup_file = |p: &std::path::Path| {
        if !scenario_existed_before {
            let _ = std::fs::remove_file(p);
        }
        if !dir_existed_before {
            let _ = std::fs::remove_dir(&dir);
        }
    };
    let mut open_opts = std::fs::OpenOptions::new();
    open_opts.write(true);
    if overwrite_mode {
        open_opts.create(true).truncate(true);
    } else {
        open_opts.create_new(true);
    }
    use std::io::Write as _;
    match open_opts.open(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(toml.as_bytes()) {
                drop(f); // close handle before remove_file
                cleanup_file(&path);
                send_err(format!("Failed to write {}: {e}", path.display()));
                return;
            }
        }
        Err(e) if !overwrite_mode && e.kind() == std::io::ErrorKind::AlreadyExists => {
            // The pre-prompt existence check missed this — must have
            // materialized between then and now. We never prompted, so
            // do NOT overwrite; just surface the conflict.
            send_err(format!(
                "{} already exists; pick another name or remove it first.",
                path.display()
            ));
            return;
        }
        Err(e) => {
            cleanup_file(&path);
            send_err(format!("Failed to open {}: {e}", path.display()));
            return;
        }
    }

    // Write the SESSION_TRACE.md sidecar. Overwrite-mode mirrors what
    // we did for scenario.toml; otherwise atomic-create-new with a
    // soft warning on AlreadyExists (the trace is reference material,
    // not load-bearing). Trace failure never rolls back scenario.toml.
    let trace_path = dir.join("SESSION_TRACE.md");
    let mut trace_opts = std::fs::OpenOptions::new();
    trace_opts.write(true);
    if overwrite_mode {
        trace_opts.create(true).truncate(true);
    } else {
        trace_opts.create_new(true);
    }
    let trace_write_error = match trace_opts.open(&trace_path) {
        Ok(mut f) => match f.write_all(session_trace.as_bytes()) {
            Ok(()) => None,
            Err(e) => Some(format!(
                "scenario.toml was written, but SESSION_TRACE.md failed: {e}"
            )),
        },
        Err(e) if !overwrite_mode && e.kind() == std::io::ErrorKind::AlreadyExists => {
            Some(format!(
                "scenario.toml was written, but {} already exists — left untouched.",
                trace_path.display()
            ))
        }
        Err(e) => Some(format!(
            "scenario.toml was written, but SESSION_TRACE.md could not be opened: {e}"
        )),
    };

    if let Some(warning) = trace_write_error {
        tracing::warn!(%warning, "session trace sidecar write failed");
        // Surface as a System message so the user sees it but the
        // scenario.toml write still counts as success.
        let _ = event_tx.send(AppEvent::ExportScenarioError { error: warning });
    }

    let _ = event_tx.send(AppEvent::ExportScenarioFinish {
        path,
        name: name.to_string(),
    });
}

/// Scenario directory names must be safe filesystem slugs. Matches the
/// convention seen under `eval/scenarios/`: lowercase ASCII letters,
/// digits, hyphens, with an optional leading underscore for scenarios
/// like `_smoke` and `_judge-smoke`. A bare `_` is rejected — a literal
/// underscore directory name would be visually confusing alongside
/// `_smoke` and friends. Max length 64 keeps slugs comfortably under
/// every common filesystem's NAME_MAX (=255 on ext4/APFS) while ruling
/// out copy-paste accidents. `Scenario::validate` doesn't enforce a
/// regex itself; this check is the single gate.
const SCENARIO_NAME_MAX_LEN: usize = 64;

fn is_valid_scenario_name(s: &str) -> bool {
    if s.is_empty() || s.len() > SCENARIO_NAME_MAX_LEN {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit() || first == '_') {
        return false;
    }
    // Track whether any body chars exist so a bare "_" is rejected even
    // though it passes the first-char check.
    let mut body_chars = 0usize;
    for c in chars {
        body_chars += 1;
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return false;
        }
    }
    !(first == '_' && body_chars == 0)
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

    #[tokio::test]
    async fn command_export_scenario_no_session_errors() {
        let mut app = make_test_app();
        app.handle_command("/export-scenario").await.unwrap();
        assert!(has_error_message(&app, "No active session to export"));
    }

    #[tokio::test]
    async fn command_export_scenario_compacted_session_errors_up_front() {
        // After `/compact`, `stored_messages` is replaced with a single
        // assistant summary (`event_loop.rs:637`). The previous
        // `is_empty()` guard passed in this state and the user only saw
        // the failure AFTER typing a name. The fix checks the actual
        // user_turns count up front and surfaces a targeted error.
        let mut app = make_test_app();
        app.stored_messages
            .push(crate::session::message::Message::assistant(
                "s",
                "compacted summary text",
            ));
        app.handle_command("/export-scenario").await.unwrap();
        assert!(
            has_error_message(&app, "no user turns to export"),
            "expected up-front user-turn error, got: {:?}",
            app.messages.last()
        );
    }

    /// Drives `export_scenario_task` directly with a synthetic event-tx
    /// pair and a tempdir as the data-dir. Covers every branch of the
    /// task except the platform-specific `ProjectDirs::from` resolution,
    /// which is handled in `handle_export_scenario` before the spawn.
    fn one_user_turn() -> Vec<String> {
        vec!["hello".to_string()]
    }

    fn spawn_task(
        data_dir: PathBuf,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
        let handle = tokio::spawn(export_scenario_task(
            data_dir,
            one_user_turn(),
            Vec::new(),
            "# Session trace\n\n(test trace body)\n".to_string(),
            tx,
        ));
        (rx, handle)
    }

    /// Drive the prompt round-trip: pull the QuestionRequest, send a name,
    /// return the follow-up event the task emits.
    async fn drive_export(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        name: &str,
    ) -> AppEvent {
        let response_tx = match rx.recv().await.expect("question") {
            AppEvent::QuestionRequest(req) => req.response_tx,
            other => panic!("expected QuestionRequest, got {other:?}"),
        };
        let _ = response_tx.send(name.to_string());
        rx.recv().await.expect("expected follow-up event")
    }

    #[tokio::test]
    async fn export_scenario_task_emits_question_request_first() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        let first = rx.recv().await.expect("first event");
        match first {
            AppEvent::QuestionRequest(req) => {
                assert_eq!(req.call_id, "slash-export-scenario");
                assert!(req.question.starts_with("Scenario name"));
                assert!(req.options.is_empty());
                // Drop req.response_tx by letting it go out of scope.
            }
            other => panic!("expected QuestionRequest, got {other:?}"),
        }
        // Dropping the response_tx makes response_rx.await return Err,
        // which the task surfaces as a session-reset abort.
        match rx.recv().await.expect("follow-up") {
            AppEvent::ExportScenarioError { error } => {
                assert!(error.contains("session reset"), "got {error:?}");
            }
            other => panic!("expected ExportScenarioError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn export_scenario_task_treats_decline_sentinel_as_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        match drive_export(&mut rx, "User declined to answer.").await {
            AppEvent::ExportScenarioError { error } => {
                assert_eq!(error, "Scenario export cancelled.");
            }
            other => panic!("expected ExportScenarioError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn export_scenario_task_rejects_invalid_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        match drive_export(&mut rx, "Has Caps And Spaces").await {
            AppEvent::ExportScenarioError { error } => {
                assert!(error.contains("Invalid scenario name"), "got {error:?}");
                assert!(
                    error.contains("Has Caps And Spaces"),
                    "expected echo, got {error:?}"
                );
            }
            other => panic!("expected ExportScenarioError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn export_scenario_task_writes_scaffold_on_happy_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        let event = drive_export(&mut rx, "my-scenario").await;
        match event {
            AppEvent::ExportScenarioFinish { path, name } => {
                assert_eq!(name, "my-scenario");
                assert_eq!(path, tmp.path().join("scenarios/my-scenario/scenario.toml"));
                assert!(path.exists(), "scenario.toml should have been written");
                let contents = std::fs::read_to_string(&path).unwrap();
                // Sanity-check the emitted scaffold: parses + has the user turn.
                let parsed = crate::eval::scenario::Scenario::from_toml_str(&contents)
                    .expect("emitted scaffold parses");
                assert_eq!(parsed.name, "my-scenario");
                assert_eq!(parsed.user_turns, vec!["hello".to_string()]);

                // Sidecar SESSION_TRACE.md must have been written next to it.
                let trace_path = path
                    .parent()
                    .expect("scenario.toml has a parent")
                    .join("SESSION_TRACE.md");
                assert!(
                    trace_path.exists(),
                    "SESSION_TRACE.md should have been written alongside scenario.toml"
                );
                let trace = std::fs::read_to_string(&trace_path).unwrap();
                assert!(
                    trace.starts_with("# Session trace"),
                    "trace file should start with the expected header, got: {trace:?}"
                );
            }
            other => panic!("expected ExportScenarioFinish, got {other:?}"),
        }
    }

    /// Drive both prompts in sequence: send a name, then answer the
    /// overwrite-confirm prompt, then return the next event the task
    /// emits.
    async fn drive_export_with_overwrite_answer(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        name: &str,
        overwrite_answer: &str,
    ) -> AppEvent {
        // Name prompt
        let name_tx = match rx.recv().await.expect("name question") {
            AppEvent::QuestionRequest(req) => req.response_tx,
            other => panic!("expected name QuestionRequest, got {other:?}"),
        };
        let _ = name_tx.send(name.to_string());
        // Overwrite prompt
        let confirm_tx = match rx.recv().await.expect("overwrite question") {
            AppEvent::QuestionRequest(req) => {
                assert_eq!(req.call_id, "slash-export-scenario-overwrite");
                assert!(
                    req.question.contains("already exists"),
                    "got prompt: {:?}",
                    req.question
                );
                assert_eq!(
                    req.options,
                    vec!["Cancel".to_string(), "Overwrite".to_string()],
                    "Cancel must be the first/default option for safety"
                );
                req.response_tx
            }
            other => panic!("expected overwrite QuestionRequest, got {other:?}"),
        };
        let _ = confirm_tx.send(overwrite_answer.to_string());
        rx.recv().await.expect("follow-up event")
    }

    #[tokio::test]
    async fn export_scenario_task_overwrite_cancel_preserves_existing() {
        // User explicitly cancels the overwrite prompt — the existing
        // scenario.toml MUST stay byte-for-byte unchanged.
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("scenarios/my-scenario");
        std::fs::create_dir_all(&scenario_dir).unwrap();
        let scenario_path = scenario_dir.join("scenario.toml");
        std::fs::write(&scenario_path, b"pre-existing content").unwrap();

        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        match drive_export_with_overwrite_answer(&mut rx, "my-scenario", "Cancel").await {
            AppEvent::ExportScenarioError { error } => {
                assert!(error.contains("cancelled"), "got {error:?}");
            }
            other => panic!("expected ExportScenarioError, got {other:?}"),
        }
        let contents = std::fs::read_to_string(&scenario_path).unwrap();
        assert_eq!(contents, "pre-existing content");
    }

    #[tokio::test]
    async fn export_scenario_task_overwrite_confirm_replaces_existing() {
        // User explicitly confirms overwrite — the existing scenario.toml
        // is replaced with the new scaffold and Finish fires.
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("scenarios/my-scenario");
        std::fs::create_dir_all(&scenario_dir).unwrap();
        let scenario_path = scenario_dir.join("scenario.toml");
        std::fs::write(&scenario_path, b"pre-existing content").unwrap();

        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        match drive_export_with_overwrite_answer(&mut rx, "my-scenario", "Overwrite").await {
            AppEvent::ExportScenarioFinish { path, .. } => {
                let contents = std::fs::read_to_string(&path).unwrap();
                assert!(
                    !contents.contains("pre-existing content"),
                    "overwrite did not replace the old content"
                );
                // The new content parses as a valid scenario.
                crate::eval::scenario::Scenario::from_toml_str(&contents)
                    .expect("overwritten scaffold parses");
            }
            other => panic!("expected ExportScenarioFinish, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn export_scenario_task_overwrite_decline_sentinel_treated_as_cancel() {
        // Esc on the overwrite prompt sends "User declined to answer."
        // The task must treat that as Cancel (no overwrite), preserving
        // the existing file.
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("scenarios/my-scenario");
        std::fs::create_dir_all(&scenario_dir).unwrap();
        let scenario_path = scenario_dir.join("scenario.toml");
        std::fs::write(&scenario_path, b"pre-existing content").unwrap();

        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        let event = drive_export_with_overwrite_answer(
            &mut rx,
            "my-scenario",
            "User declined to answer.",
        )
        .await;
        assert!(matches!(event, AppEvent::ExportScenarioError { .. }));
        let contents = std::fs::read_to_string(&scenario_path).unwrap();
        assert_eq!(contents, "pre-existing content");
    }

    #[tokio::test]
    async fn export_scenario_task_overwrite_replaces_session_trace_too() {
        // The overwrite confirmation applies to both scenario.toml AND
        // SESSION_TRACE.md — they're treated as a pair.
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("scenarios/my-scenario");
        std::fs::create_dir_all(&scenario_dir).unwrap();
        let scenario_path = scenario_dir.join("scenario.toml");
        let trace_path = scenario_dir.join("SESSION_TRACE.md");
        std::fs::write(&scenario_path, b"old scenario").unwrap();
        std::fs::write(&trace_path, b"old trace").unwrap();

        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        assert!(matches!(
            drive_export_with_overwrite_answer(&mut rx, "my-scenario", "Overwrite").await,
            AppEvent::ExportScenarioFinish { .. }
        ));
        let trace_contents = std::fs::read_to_string(&trace_path).unwrap();
        assert!(
            trace_contents.starts_with("# Session trace"),
            "trace should have been replaced, got: {trace_contents:?}"
        );
    }

    #[tokio::test]
    async fn export_scenario_task_overwrite_preserves_unrelated_files_in_dir() {
        // Fixtures the user already copied into the scenario directory
        // should survive an overwrite — only scenario.toml and
        // SESSION_TRACE.md get replaced.
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("scenarios/my-scenario");
        std::fs::create_dir_all(&scenario_dir).unwrap();
        std::fs::write(scenario_dir.join("scenario.toml"), b"old").unwrap();
        let fixture_path = scenario_dir.join("fixtures").join("data.json");
        std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
        std::fs::write(&fixture_path, b"{ \"key\": \"value\" }").unwrap();

        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        assert!(matches!(
            drive_export_with_overwrite_answer(&mut rx, "my-scenario", "Overwrite").await,
            AppEvent::ExportScenarioFinish { .. }
        ));
        let fixture_contents = std::fs::read_to_string(&fixture_path).unwrap();
        assert_eq!(fixture_contents, "{ \"key\": \"value\" }");
    }

    #[tokio::test]
    async fn export_scenario_task_preserves_preexisting_empty_dir_on_open_error() {
        // Pre-create an empty `<name>/` directory AND a directory at the
        // scenario.toml path. The path exists (as a dir) so the overwrite
        // prompt fires; even when the user confirms Overwrite, the
        // truncate-open fails with IsADirectory. The cleanup branch then
        // runs — it MUST NOT remove either the user's pre-existing
        // scenario_dir or the blocking sub-entry.
        let tmp = tempfile::tempdir().unwrap();
        let scenario_dir = tmp.path().join("scenarios/preexisting");
        std::fs::create_dir_all(&scenario_dir).unwrap();
        let blocker = scenario_dir.join("scenario.toml");
        std::fs::create_dir(&blocker).unwrap();

        let (mut rx, _h) = spawn_task(tmp.path().to_path_buf());
        let event =
            drive_export_with_overwrite_answer(&mut rx, "preexisting", "Overwrite").await;
        assert!(matches!(event, AppEvent::ExportScenarioError { .. }));
        assert!(
            scenario_dir.exists(),
            "user's pre-existing directory must not be removed on failure"
        );
        assert!(blocker.exists(), "blocking sub-entry must remain too");
    }

    // ----- Helpers that walk session state -----

    #[test]
    fn collect_user_turns_filters_and_trims_normal_alternation() {
        use crate::session::message::Message;
        // Plain alternating User/Assistant pattern with whitespace on
        // the user side. Asserts trim semantics; the alternation check
        // is exercised separately.
        let msgs = vec![
            Message::user("s", "  first  "),
            Message::assistant("s", "ignored"),
            Message::user("s", "second"),
            Message::assistant("s", "ignored"),
            Message::user("s", "third"),
        ];
        assert_eq!(
            collect_user_turns(&msgs).expect("alternation OK"),
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ]
        );
    }

    #[test]
    fn collect_user_turns_rejects_consecutive_user_messages() {
        // Mid-stream interjection: a second User message arrives without
        // an intervening Assistant turn. `handle_interjection` (see
        // `app/helpers.rs:255`) pushes interjections directly into
        // `stored_messages`, so this shape shows up in real sessions.
        // Refuse rather than produce a scenario that replays with
        // different semantics.
        use crate::session::message::Message;
        let msgs = vec![
            Message::user("s", "initial"),
            Message::assistant("s", "starts replying..."),
            Message::user("s", "wait actually"), // interjection mid-stream
            Message::user("s", "do this instead"),
        ];
        assert_eq!(
            collect_user_turns(&msgs),
            Err(UserTurnError::MidStreamInterjection)
        );
    }

    #[test]
    fn collect_user_turns_consecutive_with_empty_text_still_rejected() {
        // An interjection with whitespace-only text would otherwise be
        // dropped, but the SECOND User message that follows it (without
        // an intervening Assistant) is still a consecutive-User signal.
        // Detection must fire regardless of text content.
        use crate::session::message::Message;
        let msgs = vec![
            Message::user("s", "real first"),
            Message::assistant("s", "..."),
            Message::user("s", "   "), // empty interjection
            Message::user("s", "real second"),
        ];
        assert_eq!(
            collect_user_turns(&msgs),
            Err(UserTurnError::MidStreamInterjection)
        );
    }

    #[test]
    fn collect_user_turns_allows_system_messages_between_turns() {
        // System messages are framework-injected (`/new` system block,
        // session-started notice, etc.) and must not break the
        // User/Assistant alternation invariant.
        use crate::session::message::{Message, MessagePart, Role};
        let msgs = vec![
            Message::user("s", "hi"),
            Message {
                id: "x".into(),
                session_id: "s".into(),
                role: Role::System,
                parts: vec![MessagePart::Text {
                    text: "system note".into(),
                }],
                created_at: chrono::Utc::now(),
            },
            Message::assistant("s", "hello"),
            Message::user("s", "next"),
        ];
        assert_eq!(
            collect_user_turns(&msgs).expect("system doesn't break turns"),
            vec!["hi".to_string(), "next".to_string()]
        );
    }

    #[tokio::test]
    async fn command_export_scenario_interjection_session_errors_up_front() {
        // Sanity-check the handler-level surface for the interjection
        // case: pre-populate `stored_messages` with a U/A/U/U sequence
        // and confirm the user sees a targeted error before any prompt.
        let mut app = make_test_app();
        app.stored_messages
            .push(crate::session::message::Message::user("s", "initial"));
        app.stored_messages
            .push(crate::session::message::Message::assistant(
                "s",
                "responding...",
            ));
        app.stored_messages
            .push(crate::session::message::Message::user("s", "wait"));
        app.stored_messages
            .push(crate::session::message::Message::user(
                "s",
                "instead do this",
            ));
        app.handle_command("/export-scenario").await.unwrap();
        assert!(
            has_error_message(&app, "mid-stream interjection"),
            "expected interjection error, got: {:?}",
            app.messages.last()
        );
    }

    #[test]
    fn collect_fixture_paths_walks_ui_message_blocks() {
        use crate::ui::message_block::{
            AssistantPart, MessageBlock, ToolCall, ToolGroup, ToolGroupStatus,
        };
        let mk_call = |tool: ToolName, summary: &str, is_error: bool, completed: bool| ToolCall {
            call_id: "c".into(),
            tool_name: tool,
            args_summary: summary.into(),
            full_output: if completed { Some("ok".into()) } else { None },
            result_summary: None,
            diff_content: None,
            is_error,
            expanded: false,
            agent_progress: None,
        };
        let blocks = vec![
            MessageBlock::User { text: "hi".into() },
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::ToolGroup(ToolGroup {
                    status: ToolGroupStatus::Complete,
                    calls: vec![
                        mk_call(ToolName::Read, "src/main.rs", false, true),
                        mk_call(ToolName::Edit, "src/lib.rs", false, true),
                        // Errored: skip
                        mk_call(ToolName::Read, "missing.rs", true, true),
                        // Not completed (no full_output): skip
                        mk_call(ToolName::Read, "in-flight.rs", false, false),
                        // Non-candidate tool: skip
                        mk_call(ToolName::Bash, "ls", false, true),
                        // Absolute path: skip (validation)
                        mk_call(ToolName::Read, "/etc/passwd", false, true),
                        // .. component: skip (validation)
                        mk_call(ToolName::Read, "../secret.txt", false, true),
                        // ./ prefix: skip (validation)
                        mk_call(ToolName::Read, "./foo.rs", false, true),
                        // Read with count suffix: keep the prefix path
                        mk_call(ToolName::Read, "src/log.txt (count)", false, true),
                        // Read with tail suffix: keep the prefix path
                        mk_call(ToolName::Read, "src/log.txt (tail 50)", false, true),
                        // Read with paths array shape: skip (can't recover)
                        mk_call(ToolName::Read, "3 files", false, true),
                        // Duplicate of src/main.rs: dedup
                        mk_call(ToolName::Read, "src/main.rs", false, true),
                    ],
                })],
            },
        ];
        let paths = collect_fixture_paths(&blocks);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("src/main.rs"),
                PathBuf::from("src/lib.rs"),
                PathBuf::from("src/log.txt"),
            ]
        );
    }

    #[test]
    fn collect_fixture_paths_empty_when_no_assistant_tool_calls() {
        use crate::ui::message_block::MessageBlock;
        let blocks = vec![
            MessageBlock::User { text: "hi".into() },
            MessageBlock::System {
                text: "system note".into(),
            },
        ];
        assert!(collect_fixture_paths(&blocks).is_empty());
    }

    #[test]
    fn path_from_args_summary_handles_read_variants() {
        // Simple path
        assert_eq!(
            path_from_args_summary(ToolName::Read, "src/main.rs"),
            Some(PathBuf::from("src/main.rs"))
        );
        // Paths-array form "{n} files" → no recoverable path
        assert_eq!(path_from_args_summary(ToolName::Read, "3 files"), None);
        // Exact "(count)" suffix is stripped
        assert_eq!(
            path_from_args_summary(ToolName::Read, "src/log.txt (count)"),
            Some(PathBuf::from("src/log.txt"))
        );
        // Exact "(tail N)" suffix is stripped
        assert_eq!(
            path_from_args_summary(ToolName::Read, "src/log.txt (tail 50)"),
            Some(PathBuf::from("src/log.txt"))
        );
        // Empty
        assert_eq!(path_from_args_summary(ToolName::Read, ""), None);
    }

    #[test]
    fn path_from_args_summary_preserves_filenames_containing_parens() {
        // Real filename containing " (...)" — NOT a (count)/(tail N) suffix.
        // Must pass through verbatim, not get truncated at the first " (".
        assert_eq!(
            path_from_args_summary(ToolName::Read, "docs/foo (draft).md"),
            Some(PathBuf::from("docs/foo (draft).md"))
        );
        assert_eq!(
            path_from_args_summary(ToolName::Read, "data (2024) snapshot.json"),
            Some(PathBuf::from("data (2024) snapshot.json"))
        );
        // Edit/Write/Patch also pass parens through verbatim — they only
        // ever see the raw `file_path` value.
        assert_eq!(
            path_from_args_summary(ToolName::Edit, "src/foo (bar).rs"),
            Some(PathBuf::from("src/foo (bar).rs"))
        );
    }

    #[test]
    fn path_from_args_summary_rejects_lookalike_tail_suffix() {
        // "(tail abc)" is NOT a real suffix shape — N must be all digits.
        // Such a string must be treated as a real filename, not stripped.
        assert_eq!(
            path_from_args_summary(ToolName::Read, "log.txt (tail abc)"),
            Some(PathBuf::from("log.txt (tail abc)"))
        );
        // " (count anything)" is NOT a real suffix shape either.
        assert_eq!(
            path_from_args_summary(ToolName::Read, "log.txt (count please)"),
            Some(PathBuf::from("log.txt (count please)"))
        );
    }

    #[test]
    fn is_multi_path_read_summary_only_matches_digit_prefix() {
        assert!(is_multi_path_read_summary("3 files"));
        assert!(is_multi_path_read_summary("100 files"));
        assert!(!is_multi_path_read_summary("files"));
        assert!(!is_multi_path_read_summary(" files")); // empty digit prefix
        assert!(!is_multi_path_read_summary("3files"));
        assert!(!is_multi_path_read_summary("about-3 files")); // non-digit prefix
        assert!(!is_multi_path_read_summary("foo.txt"));
    }

    #[test]
    fn strip_tail_suffix_requires_exact_shape() {
        assert_eq!(strip_tail_suffix("log.txt (tail 50)"), Some("log.txt"));
        assert_eq!(strip_tail_suffix("a (tail 1)"), Some("a"));
        // Empty path prefix → reject.
        assert_eq!(strip_tail_suffix(" (tail 50)"), None);
        // Empty digit run → reject.
        assert_eq!(strip_tail_suffix("log.txt (tail )"), None);
        // Non-digit N → reject.
        assert_eq!(strip_tail_suffix("log.txt (tail abc)"), None);
        // Missing closing paren → reject.
        assert_eq!(strip_tail_suffix("log.txt (tail 50"), None);
        // No "(tail " at all → reject.
        assert_eq!(strip_tail_suffix("log.txt"), None);
    }

    #[test]
    fn path_from_args_summary_handles_write_class_tools() {
        for tool in [ToolName::Edit, ToolName::Write, ToolName::Patch] {
            assert_eq!(
                path_from_args_summary(tool, "src/main.rs"),
                Some(PathBuf::from("src/main.rs"))
            );
            assert_eq!(path_from_args_summary(tool, ""), None);
        }
    }

    #[test]
    fn is_fixture_candidate_tool_is_exhaustive() {
        use strum::IntoEnumIterator;
        for tool in ToolName::iter() {
            let is_candidate = is_fixture_candidate_tool(tool);
            if matches!(
                tool,
                ToolName::Read | ToolName::Edit | ToolName::Write | ToolName::Patch
            ) {
                assert!(is_candidate, "{tool:?} should be a fixture candidate");
            } else {
                assert!(!is_candidate, "{tool:?} should NOT be a fixture candidate");
            }
        }
    }

    #[test]
    fn build_session_trace_renders_turns_with_tool_calls_and_final_message() {
        use crate::ui::message_block::{
            AssistantPart, MessageBlock, ToolCall, ToolGroup, ToolGroupStatus,
        };
        let blocks = vec![
            MessageBlock::User {
                text: "look at the report and tell me Q3 revenue".into(),
            },
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![
                    AssistantPart::ToolGroup(ToolGroup {
                        status: ToolGroupStatus::Complete,
                        calls: vec![ToolCall {
                            call_id: "c".into(),
                            tool_name: ToolName::Read,
                            args_summary: "report.txt".into(),
                            full_output: Some("Q1 revenue: $10,000\nQ3 revenue: $42,331".into()),
                            result_summary: None,
                            diff_content: None,
                            is_error: false,
                            expanded: false,
                            agent_progress: None,
                        }],
                    }),
                    AssistantPart::Text("The Q3 revenue was $42,331.".into()),
                ],
            },
        ];
        let trace = build_session_trace(&blocks);
        assert!(trace.starts_with("# Session trace"));
        assert!(trace.contains("## Turn 1"));
        assert!(trace.contains("**User:**"));
        assert!(trace.contains("> look at the report"));
        assert!(trace.contains("**Tool calls:**"));
        assert!(trace.contains("`read`"));
        assert!(trace.contains("`report.txt`"));
        assert!(trace.contains("Q3 revenue: $42,331"));
        assert!(trace.contains("**Assistant:**"));
        assert!(trace.contains("> The Q3 revenue was $42,331."));
    }

    #[test]
    fn build_session_trace_truncates_long_tool_output() {
        use crate::ui::message_block::{
            AssistantPart, MessageBlock, ToolCall, ToolGroup, ToolGroupStatus,
        };
        let output: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let blocks = vec![
            MessageBlock::User { text: "hi".into() },
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::ToolGroup(ToolGroup {
                    status: ToolGroupStatus::Complete,
                    calls: vec![ToolCall {
                        call_id: "c".into(),
                        tool_name: ToolName::Read,
                        args_summary: "big.txt".into(),
                        full_output: Some(output),
                        result_summary: None,
                        diff_content: None,
                        is_error: false,
                        expanded: false,
                        agent_progress: None,
                    }],
                })],
            },
        ];
        let trace = build_session_trace(&blocks);
        assert!(trace.contains("line 1"));
        assert!(trace.contains("line 20"));
        assert!(!trace.contains("line 21"));
        assert!(trace.contains("... (30 more lines)"));
    }

    #[test]
    fn build_session_trace_marks_errored_tool_calls() {
        use crate::ui::message_block::{
            AssistantPart, MessageBlock, ToolCall, ToolGroup, ToolGroupStatus,
        };
        let blocks = vec![
            MessageBlock::User { text: "hi".into() },
            MessageBlock::Assistant {
                thinking: None,
                parts: vec![AssistantPart::ToolGroup(ToolGroup {
                    status: ToolGroupStatus::Complete,
                    calls: vec![ToolCall {
                        call_id: "c".into(),
                        tool_name: ToolName::Read,
                        args_summary: "missing.txt".into(),
                        full_output: Some("Error: not found".into()),
                        result_summary: None,
                        diff_content: None,
                        is_error: true,
                        expanded: false,
                        agent_progress: None,
                    }],
                })],
            },
        ];
        let trace = build_session_trace(&blocks);
        assert!(trace.contains("[ERROR]"));
    }

    #[test]
    fn build_session_trace_skips_system_and_other_block_kinds() {
        // System / Error / Permission / Question blocks are framework
        // chatter and must NOT show up in the trace.
        use crate::ui::message_block::MessageBlock;
        let blocks = vec![
            MessageBlock::System {
                text: "session started".into(),
            },
            MessageBlock::User { text: "hi".into() },
            MessageBlock::Error {
                text: "some error".into(),
            },
            MessageBlock::Permission {
                tool_name: "bash".into(),
                args_summary: "rm -rf /".into(),
                diff_content: None,
            },
        ];
        let trace = build_session_trace(&blocks);
        assert!(!trace.contains("session started"));
        assert!(!trace.contains("some error"));
        assert!(!trace.contains("rm -rf"));
        assert!(trace.contains("**User:**"));
    }

    #[test]
    fn is_valid_scenario_name_accepts_canonical_slugs() {
        assert!(is_valid_scenario_name("recover-after-edit"));
        assert!(is_valid_scenario_name("a"));
        assert!(is_valid_scenario_name("scenario-1"));
        assert!(is_valid_scenario_name("0-leading-digit-ok"));
        // Leading underscore is intentional — matches the convention of
        // `_smoke` and `_judge-smoke` under eval/scenarios/.
        assert!(is_valid_scenario_name("_smoke"));
        assert!(is_valid_scenario_name("_judge-smoke"));
    }

    #[test]
    fn is_valid_scenario_name_rejects_bad_slugs() {
        assert!(!is_valid_scenario_name(""));
        assert!(!is_valid_scenario_name("-leading-hyphen"));
        assert!(!is_valid_scenario_name("Capital"));
        assert!(!is_valid_scenario_name("has space"));
        assert!(!is_valid_scenario_name("has/slash"));
        // Underscores are NOT allowed in the body, only as the first char.
        assert!(!is_valid_scenario_name("has_underscore"));
        assert!(!is_valid_scenario_name("_has_underscore_inside"));
        assert!(!is_valid_scenario_name("has.dot"));
        assert!(!is_valid_scenario_name(".."));
        assert!(!is_valid_scenario_name("emoji-\u{1f600}"));
        // Bare "_" is rejected — a literal underscore directory is
        // visually confusing alongside the `_smoke` convention.
        assert!(!is_valid_scenario_name("_"));
        // Too long: NAME_MAX is 255 on ext4/APFS but 64 is a sane
        // human-readable cap that catches copy-paste accidents.
        let too_long = "a".repeat(SCENARIO_NAME_MAX_LEN + 1);
        assert!(!is_valid_scenario_name(&too_long));
        // Exactly the max length is accepted.
        let max_len = "a".repeat(SCENARIO_NAME_MAX_LEN);
        assert!(is_valid_scenario_name(&max_len));
    }
}
