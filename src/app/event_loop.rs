use super::*;

/// Prefix and suffix of the `" (×N)"` counter that the StreamNotice dedupe
/// path appends when folding repeated identical notices into a single
/// MessageBlock. Hoisted to module scope so the writer (`format_with_count`)
/// and parser (`split_count_suffix`) share a single source of truth — if
/// the cosmetic format ever changes, both sides update together.
const COUNT_PREFIX: &str = " (×";
const COUNT_SUFFIX: &str = ")";

/// Render a System message text with an explicit count suffix. Inverse of
/// `split_count_suffix`: `split_count_suffix(&format_with_count(base, n))`
/// always returns `(base, n)` for any non-empty `base` that doesn't itself
/// end with a count suffix.
fn format_with_count(base: &str, count: usize) -> String {
    format!("{base}{COUNT_PREFIX}{count}{COUNT_SUFFIX}")
}

/// Split a System message text into `(base, count)` if it ends with the
/// `" (×N)"` counter suffix written by `format_with_count`. Returns
/// `(s, 1)` for any string that doesn't match the exact format.
///
/// The suffix is matched on the LAST `" (×"` occurrence, so a base text
/// that happens to contain the prefix earlier is preserved. The closing
/// `)` must terminate the string with no trailing content, and the digits
/// must be a well-formed `usize`.
fn split_count_suffix(s: &str) -> (&str, usize) {
    let Some(idx) = s.rfind(COUNT_PREFIX) else {
        return (s, 1);
    };
    let after = &s[idx + COUNT_PREFIX.len()..];
    let Some(close) = after.find(COUNT_SUFFIX) else {
        return (s, 1);
    };
    let trailing = &after[close + COUNT_SUFFIX.len()..];
    if !trailing.is_empty() {
        return (s, 1);
    }
    let digits = &after[..close];
    if digits.is_empty() {
        return (s, 1);
    }
    let Ok(n) = digits.parse::<usize>() else {
        return (s, 1);
    };
    (&s[..idx], n)
}

impl App {
    pub async fn run(&mut self) -> Result<()> {
        // Try to resume the last session
        self.resume_or_new_session();

        let (mut terminal, detected) = ui::detect_and_setup_terminal()?;
        self.theme = ui::terminal_detect::resolve_theme(self.config.theme, detected);
        let mut crossterm_events = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));

        // Start LSP servers in background (non-blocking). The sidebar picks up
        // state transitions via Tick polling of the shared status cache, so we
        // don't need to push an event here — the first Tick after init flips
        // entries from Starting → Ready/Indexing/Error.
        {
            let lsp = self.lsp_manager.clone();
            let tx = self.event_tx.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(mut mgr) = lsp.write() {
                    mgr.start_servers();
                    let status = mgr.language_status();
                    let running: Vec<&str> = status
                        .iter()
                        .filter(|(_, r)| *r)
                        .map(|(l, _)| l.as_str())
                        .collect();
                    if !running.is_empty() {
                        let _ = tx.send(AppEvent::StreamNotice {
                            text: format!("LSP servers started: {}", running.join(", ")),
                        });
                    }
                }
            });
        }

        // Start MCP servers in background (non-blocking)
        if !self.config.mcp_servers.is_empty() {
            let mcp = self.mcp_manager.clone();
            let tx = self.event_tx.clone();
            let configs = self.config.mcp_servers.clone();
            tokio::spawn(async move {
                let data_dir = directories::ProjectDirs::from("", "", "steve")
                    .map(|d| d.data_dir().to_path_buf());

                // Create a channel for OAuth status messages → TUI StreamNotice events
                let (oauth_tx, mut oauth_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                let notice_tx = tx.clone();
                tokio::spawn(async move {
                    while let Some(msg) = oauth_rx.recv().await {
                        let _ = notice_tx.send(AppEvent::StreamNotice { text: msg });
                    }
                });

                let mut mgr = mcp.lock().await;
                mgr.start_servers(&configs, data_dir.as_deref(), Some(oauth_tx))
                    .await;
                let summary = mgr.server_summary();
                if !summary.is_empty() {
                    let _ = tx.send(AppEvent::StreamNotice {
                        text: format!("MCP servers started: {}", summary.join(", ")),
                    });
                }
                // Update sidebar with MCP server status (connected + failed)
                let status = mgr.server_status();
                if !status.is_empty() {
                    let _ = tx.send(AppEvent::McpStatus { servers: status });
                }
            });
        }

        // If providers exist but no model is selected, prompt the user to pick one
        self.prompt_model_if_needed();

        // Initial render
        let completed = terminal.draw(|frame| ui::render(frame, self))?;
        ui::write_osc8_hyperlinks(completed.buffer, completed.area);
        // Emit once — CWD is fixed for the session (no /cd command).
        ui::write_osc7_cwd(&self.project.cwd);

        loop {
            tokio::select! {
                maybe_event = crossterm_events.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        self.handle_event(AppEvent::Input(event)).await?;
                    }
                }
                maybe_event = self.event_rx.recv() => {
                    if let Some(event) = maybe_event {
                        self.handle_event(event).await?;
                    }
                }
                _ = tick_interval.tick() => {
                    self.handle_event(AppEvent::Tick).await?;
                }
            }

            let completed = terminal.draw(|frame| ui::render(frame, self))?;
            ui::write_osc8_hyperlinks(completed.buffer, completed.area);

            if self.should_quit {
                break;
            }
        }

        // Prune the session if the user never sent a message
        self.prune_empty_session();

        ui::restore_terminal(&mut terminal)?;
        Ok(())
    }

    /// Drive the event loop without TUI rendering. Loops while
    /// `streaming_active` is `true`, returning as soon as it flips to `false`
    /// (typically when `LlmFinish` or `LlmError` reaches `finish_stream`,
    /// but also returns immediately if `streaming_active` is already `false`
    /// on entry — there is no stream to wait for).
    ///
    /// `observer` is invoked for each `AppEvent` BEFORE `handle_event`
    /// processes it, so callers can record the event without racing the
    /// internal state updates that follow.
    ///
    /// Caller is expected to kick off a stream first (e.g. via
    /// `App::handle_input`) so the loop has something to drain.
    pub async fn run_until_idle<F>(&mut self, mut observer: F) -> Result<()>
    where
        F: FnMut(&AppEvent),
    {
        while self.streaming_active {
            let event = self.event_rx.recv().await.ok_or_else(|| {
                anyhow::anyhow!("event channel closed mid-stream during run_until_idle")
            })?;
            observer(&event);
            self.handle_event(event).await?;
        }
        Ok(())
    }

    pub(super) async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Input(Event::Key(key)) => self.handle_key(key).await?,
            AppEvent::Input(Event::Mouse(mouse)) => {
                use crossterm::event::MouseButton;
                // Block mouse events in the message area when an overlay is active
                if self.model_picker.visible
                    || self.session_picker.visible
                    || self.diagnostics_overlay.visible
                    || self.mcp_overlay.visible
                    || self.pending_question.is_some()
                {
                    return Ok(());
                }
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.message_area_state.scroll_down(3),
                    MouseEventKind::ScrollUp => self.message_area_state.scroll_up(3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let area = self.last_message_area;
                        if mouse.row >= area.y
                            && mouse.row < area.y + area.height
                            && mouse.column >= area.x
                            && mouse.column < area.x + area.width
                        {
                            // Clear any previous selection and start a new drag
                            self.selection_state.clear();
                            if let Some(map) = &self.message_area_state.content_map
                                && let Some(pos) = map.screen_to_content(
                                    mouse.row,
                                    mouse.column,
                                    self.message_area_state.scroll_offset,
                                    area.y,
                                    area.x,
                                )
                            {
                                self.selection_state.anchor = Some(pos);
                                self.selection_state.cursor = Some(pos);
                                self.selection_state.dragging = true;
                            }
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) if self.selection_state.dragging => {
                        let area = self.last_message_area;
                        // Scroll-to-select: scroll when dragging past edges
                        if mouse.row < area.y {
                            self.message_area_state.scroll_up(1);
                        } else if mouse.row >= area.y + area.height {
                            self.message_area_state.scroll_down(1);
                        }
                        if let Some(map) = &self.message_area_state.content_map
                            && let Some(pos) = map.screen_to_content(
                                mouse.row,
                                mouse.column,
                                self.message_area_state.scroll_offset,
                                area.y,
                                area.x,
                            )
                        {
                            self.selection_state.cursor = Some(pos);
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) if self.selection_state.dragging => {
                        self.selection_state.dragging = false;
                        // If we have a valid selection range, copy to clipboard
                        if let Some((start, end)) = self.selection_state.ordered_range()
                            && let Some(map) = &self.message_area_state.content_map
                        {
                            let text = map.extract_text(&start, &end);
                            if !text.is_empty() {
                                self.copy_to_clipboard(&text);
                            }
                        }
                    }
                    _ => {}
                }
            }
            AppEvent::Input(Event::Paste(text))
                if self.pending_permission.is_none() && self.pending_question.is_none() =>
            {
                self.input.collapse_paste(&text);
                let current_text = self.input.textarea.lines().join("\n");
                self.autocomplete_state.update(&current_text);
            }
            AppEvent::Input(Event::Resize(_, _)) => {}
            AppEvent::Tick => {
                self.status_line_state.tick();
                self.sidebar_state.advance_spinner();
                // Poll the shared LSP status cache directly via the cloned
                // Arc — NOT via `lsp_manager.try_read()`. The startup
                // `spawn_blocking` holds the `RwLock<LspManager>` write lock
                // for the entire duration of every server's Initialize
                // (seconds for rust-analyzer), so a `try_read()` here would
                // fail throughout startup and the user would never see
                // Starting/Indexing — only the final Ready/Error state.
                let next: Vec<SidebarLsp> =
                    crate::lsp::LspManager::snapshot_cache(&self.lsp_status_cache)
                        .into_iter()
                        .map(|(_, entry)| SidebarLsp {
                            binary: entry.binary,
                            state: entry.state,
                            progress_message: entry.progress_message,
                            next_restart_at: entry.next_restart_at,
                        })
                        .collect();
                if next != self.sidebar_state.lsp_servers {
                    self.sidebar_state.lsp_servers = next;
                }
                // Clear expired "Copied!" flash
                if let Some(t) = self.selection_state.copied_flash
                    && t.elapsed().as_secs() >= 1
                {
                    self.selection_state.copied_flash = None;
                }
            }

            // -- Streaming events --
            AppEvent::LlmResponseStart if self.streaming_active => {
                // Save the completed assistant message from the previous response
                if let Some(msg) = self.streaming_message.take()
                    && !msg.text_content().is_empty()
                {
                    let mgr = SessionManager::new(&self.storage, &self.project.id);
                    let _ = mgr.save_message(&msg);
                    self.stored_messages.push(msg);
                }

                // Start a fresh assistant block for the interjection response
                if let Some(session) = &self.current_session {
                    self.streaming_message = Some(Message::assistant(&session.id, ""));
                }
                self.messages.push(MessageBlock::Assistant {
                    thinking: None,
                    parts: vec![],
                });
            }
            AppEvent::LlmDelta { text } if self.streaming_active => {
                // Append to the display message
                if let Some(last) = self.last_assistant_mut() {
                    last.append_text(&text);
                }
                // Also append to the in-progress Message for persistence
                if let Some(msg) = &mut self.streaming_message {
                    msg.append_text(&text);
                }
                self.message_area_state.scroll_to_bottom();
            }

            AppEvent::LlmReasoning { text } if self.streaming_active => {
                if let Some(last) = self.last_assistant_mut() {
                    last.append_thinking(&text);
                }
                self.message_area_state.scroll_to_bottom();
            }

            // -- Tool events --
            AppEvent::LlmToolCallStreaming {
                count: _,
                tool_name,
            } => {
                if let Some(last) = self.last_assistant_mut() {
                    last.ensure_preparing_tool_group();
                }
                self.status_line_state.set_activity(Activity::RunningTool {
                    tool_name,
                    args_summary: String::new(),
                });
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::LlmToolCall {
                call_id,
                tool_name,
                arguments,
            } => {
                let args_summary = extract_args_summary(tool_name, &arguments);
                let diff_content = extract_diff_content(tool_name, &arguments);
                if let Some(last) = self.last_assistant_mut() {
                    last.add_tool_call(
                        call_id.clone(),
                        tool_name,
                        args_summary.clone(),
                        diff_content,
                    );
                }
                self.status_line_state.set_activity(Activity::RunningTool {
                    tool_name,
                    args_summary,
                });
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::ToolResult {
                call_id,
                tool_name,
                output,
            } => {
                let summary = crate::truncate_chars(&output.output, 80);

                if let Some(last) = self.last_assistant_mut() {
                    last.complete_tool_call(
                        &call_id,
                        summary,
                        output.output.clone(),
                        output.is_error,
                    );
                }

                // On successful write tool completion: invalidate file index + record changeset
                if tool_name.is_write_tool() && !output.is_error {
                    self.invalidate_file_index();
                    if let Some(call) = self.find_last_completed_call(tool_name)
                        && let Some(diff) = &call.diff_content
                    {
                        let (additions, removals) = count_diff_lines(diff);
                        let display_path = self.strip_project_root(&call.args_summary);
                        self.sidebar_state
                            .record_file_change(display_path, additions, removals);
                    }
                }

                // Track task completions for sidebar display
                if tool_name == ToolName::Task
                    && !output.is_error
                    && let Some(call) = self.find_last_completed_call(tool_name)
                    && call.args_summary == "complete"
                {
                    // Parse task ID from output: "Completed task {id}: {title}"
                    // Safe to split on ':' — task IDs are hex-only (task-XXXXXXXX)
                    if let Some(rest) = output.output.strip_prefix("Completed task ")
                        && let Some(id) = rest.split(':').next()
                    {
                        self.sidebar_state.record_task_closed(id.trim().to_string());
                    }
                }

                // Refresh git dirty status after write tools or bash (may have committed, etc.)
                if (tool_name.is_write_tool() || tool_name == ToolName::Bash) && !output.is_error {
                    self.refresh_git_info();
                }

                self.update_sidebar();
                self.message_area_state.scroll_to_bottom();
            }

            AppEvent::LlmFinish { usage } => {
                self.finish_stream();

                // Remove trailing empty assistant message if present
                if let Some(last) = self.messages.last()
                    && last.is_empty_assistant()
                {
                    self.messages.pop();
                }

                // Save the completed assistant message to storage
                if let Some(msg) = self.streaming_message.take() {
                    let mgr = SessionManager::new(&self.storage, &self.project.id);
                    let _ = mgr.save_message(&msg);
                    self.stored_messages.push(msg.clone());

                    // Update session usage (cumulative for storage).
                    // Note: last_prompt_tokens is NOT set here — LlmUsageUpdate
                    // already set the correct per-call value. The usage here is
                    // accumulated across all loop iterations, which is wrong for
                    // context pressure display but correct for cumulative cost.
                    if let (Some(u), Some(session)) = (usage, &mut self.current_session) {
                        let _ = mgr.add_usage(session, u.prompt_tokens, u.completion_tokens);
                    } else if let Some(session) = &mut self.current_session {
                        let _ = mgr.touch_session(session);
                    }

                    // Auto-generate title after first user+assistant exchange
                    self.exchange_count += 1;
                    if self.exchange_count == 1 {
                        self.maybe_generate_title();
                    }

                    self.sync_sidebar_tokens();
                    self.sync_diagnostics();
                    self.update_sidebar();

                    // Check context usage and warn if approaching limits
                    self.check_context_warning();

                    // Check if auto-compact should trigger
                    if self.should_auto_compact() {
                        tracing::info!("auto-compact threshold reached, triggering compaction");
                        let _ = self.handle_command("/compact").await;
                    }
                }
            }
            AppEvent::LlmUsageUpdate { usage } => {
                // Update token counters mid-loop without saving to storage.
                // usage.prompt_tokens is the current call's prompt tokens (context pressure).
                self.last_prompt_tokens = usage.prompt_tokens as u64;
                self.status_line_state.last_prompt_tokens = usage.prompt_tokens as u64;
                // Accumulate into sidebar display counters for live updates.
                // session.token_usage is NOT updated here (happens on LlmFinish to
                // avoid intermediate disk writes). update_sidebar() will overwrite
                // these with authoritative session values after LlmFinish.
                self.sidebar_state.prompt_tokens += usage.prompt_tokens as u64;
                self.sidebar_state.completion_tokens += usage.completion_tokens as u64;
                self.sidebar_state.total_tokens +=
                    (usage.prompt_tokens + usage.completion_tokens) as u64;
                self.check_context_warning();
            }
            AppEvent::LlmRetry {
                attempt,
                max_attempts,
                error,
            } => {
                self.messages.push(MessageBlock::System {
                    text: format!(
                        "Connection error: {error}\nRetrying ({attempt}/{max_attempts})..."
                    ),
                });
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::LlmError { error } => {
                self.finish_stream();
                self.streaming_message = None;
                self.messages.push(MessageBlock::Error { text: error });
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::StreamNotice { text } => {
                // Coalesce identical consecutive System notices into a single
                // block with a "(×N)" counter. Without this, a noisy LSP
                // restart loop (or any other source of repeated transient
                // notices) appends a fresh block each time and quickly scrolls
                // earlier user/assistant content out of view.
                let folded = match self.messages.last_mut() {
                    Some(MessageBlock::System { text: prev }) => {
                        // Belt-and-suspenders: try literal equality first so a
                        // notice whose text itself ends with `" (×N)"` still
                        // coalesces correctly when repeated verbatim, without
                        // depending on the parser's interpretation of that
                        // tail. Falls back to the parse-and-fold path for the
                        // common `"X" → "X (×2)"` transition.
                        if *prev == text {
                            *prev = format_with_count(&text, 2);
                            true
                        } else {
                            let (base, count) = split_count_suffix(prev);
                            if base == text {
                                // saturating_add: a counter that pins at
                                // usize::MAX is correct degenerate behavior —
                                // additional folds become silent no-ops and
                                // the display freezes, rather than panicking
                                // (debug) or wrapping (release). Defense
                                // against malformed input that already has a
                                // near-MAX count baked in.
                                let next = count.saturating_add(1);
                                *prev = format_with_count(base, next);
                                true
                            } else {
                                false
                            }
                        }
                    }
                    _ => false,
                };
                if !folded {
                    self.messages.push(MessageBlock::System { text });
                }
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::AgentProgress {
                call_id,
                tool_name,
                args_summary,
                result_summary,
            } => {
                // Update the specific agent tool call's inline progress by call_id.
                if let Some(last) = self.last_assistant_mut() {
                    if result_summary.is_some() {
                        last.update_agent_progress_result(&call_id, result_summary);
                    } else {
                        last.update_agent_progress(&call_id, tool_name, args_summary);
                    }
                }
            }
            AppEvent::McpStatus { servers } => {
                self.sidebar_state.mcp_servers = servers;
            }
            AppEvent::PermissionRequest(req) => {
                // Show permission prompt to user, with diff preview if available
                let diff_content = extract_diff_content(req.tool_name, &req.tool_args);
                let shown_name = req
                    .display_name
                    .clone()
                    .unwrap_or_else(|| req.tool_name.to_string());
                self.messages.push(MessageBlock::Permission {
                    tool_name: shown_name,
                    args_summary: req.arguments_summary.clone(),
                    diff_content,
                });
                self.status_line_state
                    .set_activity(Activity::WaitingForPermission);
                self.message_area_state.scroll_to_bottom();
                self.pending_permission = Some(PendingPermission {
                    tool_name: req.tool_name,
                    summary: req.arguments_summary,
                    response_tx: req.response_tx,
                });
            }
            AppEvent::QuestionRequest(req) => {
                let has_options = !req.options.is_empty();
                self.messages.push(MessageBlock::Question {
                    question: req.question.clone(),
                    options: req.options.clone(),
                    selected: if has_options { Some(0) } else { None },
                    free_text: String::new(),
                    answered: None,
                });
                self.status_line_state
                    .set_activity(Activity::WaitingForQuestion);
                self.message_area_state.scroll_to_bottom();
                self.pending_question = Some(PendingQuestion {
                    call_id: req.call_id,
                    question: req.question,
                    options: req.options,
                    selected: if has_options { Some(0) } else { None },
                    free_text: String::new(),
                    response_tx: req.response_tx,
                });
            }
            AppEvent::CompactFinish { summary } => {
                self.is_loading = false;
                self.compaction_count += 1;

                let session_id = self
                    .current_session
                    .as_ref()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();

                let mgr = SessionManager::new(&self.storage, &self.project.id);

                // 1. Delete all old messages from storage
                if let Err(e) = mgr.delete_messages(&session_id) {
                    tracing::error!(error = %e, "failed to delete old messages during compact");
                }

                // 2. Create and save a summary assistant message
                let summary_msg = Message::assistant(&session_id, &summary);
                let _ = mgr.save_message(&summary_msg);

                // 3. Reset token usage on the session
                if let Some(session) = &mut self.current_session {
                    let _ = mgr.reset_usage(session);
                }

                // 4. Replace in-memory stored_messages
                self.stored_messages = vec![summary_msg];

                // 5. Replace display messages with a system notice + the summary
                self.messages.clear();
                self.messages.push(MessageBlock::System {
                    text: "Conversation compacted.".into(),
                });
                self.messages.push(MessageBlock::Assistant {
                    thinking: None,
                    parts: vec![AssistantPart::Text(summary)],
                });
                self.message_area_state.scroll_to_bottom();
                // Reset context warning and prompt tokens since conversation is fresh
                self.context_warned = false;
                self.last_prompt_tokens = 0;
                self.sync_sidebar_tokens();
                self.sync_diagnostics();
                self.update_sidebar();

                tracing::info!("conversation compacted successfully");
            }
            AppEvent::CompactError { error } => {
                self.is_loading = false;
                self.auto_compact_failed = true;
                self.messages.push(MessageBlock::Error { text: error });
                self.status_line_state.set_activity(Activity::Idle);
                self.message_area_state.scroll_to_bottom();
                tracing::error!("compaction failed, auto-compact disabled for this session");
            }
            AppEvent::AgentsUpdateFinish { proposed_content } => {
                // Guard: if cancelled (is_loading already cleared), discard the late result
                if !self.is_loading {
                    tracing::info!(
                        "AGENTS.md update result arrived after cancellation, discarding"
                    );
                } else {
                    self.is_loading = false;
                    self.status_line_state.set_activity(Activity::Idle);
                    self.pending_agents_update = Some(proposed_content.clone());
                    self.messages.push(MessageBlock::System {
                        text: "Proposed AGENTS.md update \u{2014} press **y** to apply, **n** to discard:".to_string(),
                    });
                    self.messages.push(MessageBlock::Assistant {
                        thinking: None,
                        parts: vec![AssistantPart::Text(proposed_content)],
                    });
                    self.message_area_state.scroll_to_bottom();
                    tracing::info!("AGENTS.md update proposed, awaiting user approval");
                }
            }
            AppEvent::AgentsUpdateError { error } => {
                if !self.is_loading {
                    tracing::info!("AGENTS.md update error arrived after cancellation, discarding");
                } else {
                    self.is_loading = false;
                    self.status_line_state.set_activity(Activity::Idle);
                    self.messages.push(MessageBlock::Error { text: error });
                    self.message_area_state.scroll_to_bottom();
                    tracing::error!("AGENTS.md update failed");
                }
            }
            AppEvent::TitleGenerated { session_id, title } => {
                self.apply_title_if_current(&session_id, &title);
            }
            AppEvent::TitleError {
                session_id,
                fallback_title,
            } => {
                self.apply_title_if_current(&session_id, &fallback_title);
            }
            AppEvent::LspRestartNeeded { lang } => {
                let lsp = self.lsp_manager.clone();
                let tx = self.event_tx.clone();
                tokio::task::spawn_blocking(move || {
                    // Match the rest of the codebase: recover from poisoning
                    // rather than silently dropping the restart request.
                    let mut mgr = lsp.write().unwrap_or_else(|p| p.into_inner());
                    match mgr.restart_server(lang) {
                        Ok(()) => {
                            let _ = tx.send(AppEvent::StreamNotice {
                                text: format!("LSP {lang} server restarted successfully"),
                            });
                        }
                        Err(e) => {
                            tracing::warn!("LSP restart of {lang} failed: {e:#}");
                        }
                    }
                });
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tests::{has_error_message, has_system_message, make_test_app},
        event::{AppEvent, StreamUsage},
    };

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

        app.handle_event(AppEvent::LlmDelta {
            text: "Hello".into(),
        })
        .await
        .unwrap();
        app.handle_event(AppEvent::LlmDelta {
            text: " world".into(),
        })
        .await
        .unwrap();

        match app.messages.last().unwrap() {
            MessageBlock::Assistant { parts, .. } => {
                let text: String = parts
                    .iter()
                    .filter_map(|p| {
                        if let AssistantPart::Text(t) = p {
                            Some(t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(text, "Hello world");
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn event_llm_delta_ignored_when_not_streaming() {
        let mut app = make_test_app();
        let msg_count = app.messages.len();

        app.handle_event(AppEvent::LlmDelta {
            text: "ignored".into(),
        })
        .await
        .unwrap();

        // No new messages, no panic
        assert_eq!(app.messages.len(), msg_count);
    }

    #[tokio::test]
    async fn event_llm_response_start_creates_new_assistant_block() {
        let mut app = make_test_app();
        start_streaming(&mut app);

        // Simulate first response
        app.handle_event(AppEvent::LlmDelta {
            text: "First response".into(),
        })
        .await
        .unwrap();

        let msg_count = app.messages.len();

        // LlmResponseStart should push a new empty Assistant block
        app.handle_event(AppEvent::LlmResponseStart).await.unwrap();

        assert_eq!(app.messages.len(), msg_count + 1);
        assert!(app.messages.last().unwrap().is_empty_assistant());

        // New deltas should go to the NEW block, not the first one
        app.handle_event(AppEvent::LlmDelta {
            text: "Second response".into(),
        })
        .await
        .unwrap();

        // First assistant block should still have only "First response"
        let first_text = match &app.messages[msg_count - 1] {
            MessageBlock::Assistant { parts, .. } => parts
                .iter()
                .filter_map(|p| {
                    if let AssistantPart::Text(t) = p {
                        Some(t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<String>(),
            other => panic!("expected Assistant, got {other:?}"),
        };
        assert_eq!(first_text, "First response");

        // Last assistant block should have "Second response"
        let second_text = match app.messages.last().unwrap() {
            MessageBlock::Assistant { parts, .. } => parts
                .iter()
                .filter_map(|p| {
                    if let AssistantPart::Text(t) = p {
                        Some(t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<String>(),
            other => panic!("expected Assistant, got {other:?}"),
        };
        assert_eq!(second_text, "Second response");
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
        })
        .await
        .unwrap();

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

        app.handle_event(AppEvent::LlmError {
            error: "connection failed".into(),
        })
        .await
        .unwrap();

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

        app.handle_event(AppEvent::LlmFinish { usage: None })
            .await
            .unwrap();

        assert!(!app.is_loading);
        assert!(!app.streaming_active);
        assert!(app.frozen_elapsed.is_some());
    }

    #[tokio::test]
    async fn event_stream_notice_pushes_system_message() {
        let mut app = make_test_app();

        app.handle_event(AppEvent::StreamNotice {
            text: "LSP started".into(),
        })
        .await
        .unwrap();

        assert!(has_system_message(&app, "LSP started"));
    }

    #[tokio::test]
    async fn event_stream_notice_coalesces_consecutive_identical_text() {
        // Regression for steve-5taz: a noisy LSP restart loop must not flood
        // the messages window with N copies of the same notice. The first
        // notice pushes a fresh System block; subsequent identical notices
        // update that block in place with a "(×N)" counter.
        let mut app = make_test_app();
        let baseline = app.messages.len();

        for _ in 0..5 {
            app.handle_event(AppEvent::StreamNotice {
                text: "LSP yaml server restarted successfully".into(),
            })
            .await
            .unwrap();
        }

        assert_eq!(
            app.messages.len(),
            baseline + 1,
            "five identical notices should collapse to a single block"
        );
        match app.messages.last() {
            Some(MessageBlock::System { text }) => {
                assert_eq!(text, "LSP yaml server restarted successfully (×5)");
            }
            other => panic!("expected coalesced System block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn event_stream_notice_does_not_coalesce_distinct_text() {
        let mut app = make_test_app();
        let baseline = app.messages.len();

        app.handle_event(AppEvent::StreamNotice {
            text: "alpha".into(),
        })
        .await
        .unwrap();
        app.handle_event(AppEvent::StreamNotice {
            text: "beta".into(),
        })
        .await
        .unwrap();

        assert_eq!(
            app.messages.len(),
            baseline + 2,
            "distinct notices should produce distinct blocks"
        );
        assert!(has_system_message(&app, "alpha"));
        assert!(has_system_message(&app, "beta"));
    }

    #[tokio::test]
    async fn event_stream_notice_does_not_coalesce_through_other_block() {
        // If a non-System block lands between two identical notices, the
        // second notice creates a fresh System block (it can't reach back
        // past the intervening block).
        let mut app = make_test_app();
        let baseline = app.messages.len();

        app.handle_event(AppEvent::StreamNotice { text: "X".into() })
            .await
            .unwrap();
        app.messages.push(MessageBlock::User {
            text: "user typed something".into(),
        });
        app.handle_event(AppEvent::StreamNotice { text: "X".into() })
            .await
            .unwrap();

        assert_eq!(
            app.messages.len(),
            baseline + 3,
            "intervening non-System block prevents coalescing"
        );
    }

    #[test]
    fn split_count_suffix_no_suffix() {
        assert_eq!(split_count_suffix("Hello"), ("Hello", 1));
    }

    #[test]
    fn split_count_suffix_basic_count() {
        assert_eq!(split_count_suffix("Hello (×2)"), ("Hello", 2));
        assert_eq!(split_count_suffix("Hello (×42)"), ("Hello", 42));
    }

    #[test]
    fn split_count_suffix_rejects_invalid_shapes() {
        // Missing close paren
        assert_eq!(split_count_suffix("Hello (×2"), ("Hello (×2", 1));
        // Trailing content after the close paren
        assert_eq!(
            split_count_suffix("Hello (×2) more"),
            ("Hello (×2) more", 1)
        );
        // Non-digit content
        assert_eq!(split_count_suffix("Hello (×x)"), ("Hello (×x)", 1));
        // Empty digits
        assert_eq!(split_count_suffix("Hello (×)"), ("Hello (×)", 1));
        // Plain "x" not "×" (multiplication sign)
        assert_eq!(split_count_suffix("Hello (x2)"), ("Hello (x2)", 1));
    }

    #[test]
    fn split_count_suffix_handles_utf8_base() {
        // The base text contains multi-byte characters — slicing must land
        // on a UTF-8 boundary, not split a character.
        assert_eq!(split_count_suffix("café (×3)"), ("café", 3));
        assert_eq!(
            split_count_suffix("LSP yaml server restarted successfully (×100)"),
            ("LSP yaml server restarted successfully", 100)
        );
    }

    #[test]
    fn split_count_suffix_uses_last_prefix_match() {
        // A base text that itself contains " (×" should split on the LAST
        // occurrence so the count parses against the genuine suffix.
        assert_eq!(
            split_count_suffix("inner (×note) (×4)"),
            ("inner (×note)", 4)
        );
        // Multiple valid-looking suffixes — `rfind` must grab the rightmost.
        assert_eq!(
            split_count_suffix("a (×1) b (×2) c (×3)"),
            ("a (×1) b (×2) c", 3)
        );
        // A genuine count to the right of an invalid `" (×note)"` cluster.
        assert_eq!(
            split_count_suffix("first (×nope) middle (×7)"),
            ("first (×nope) middle", 7)
        );
    }

    #[test]
    fn format_with_count_and_split_roundtrip() {
        // Property: parse(format(base, n)) == (base, n) for any base that
        // doesn't already end with the count format. Locks the writer and
        // parser to a single source-of-truth pair.
        for &(base, n) in &[
            ("Hello", 2usize),
            ("LSP yaml server restarted successfully", 12),
            ("café", 999),
            ("", 1),
        ] {
            let rendered = format_with_count(base, n);
            assert_eq!(
                split_count_suffix(&rendered),
                (base, n),
                "roundtrip failed for ({base:?}, {n})"
            );
        }
    }

    #[tokio::test]
    async fn event_stream_notice_coalesces_empty_text() {
        // Defense-in-depth: emit sites are not expected to send empty
        // notices, but the dedupe path should still behave correctly if
        // they do — `("", 2)` round-trips through format/split, and the
        // handler folds two empty notices into a single block displaying
        // the suffix only (`" (×2)"`).
        let mut app = make_test_app();
        let baseline = app.messages.len();

        app.handle_event(AppEvent::StreamNotice {
            text: String::new(),
        })
        .await
        .unwrap();
        app.handle_event(AppEvent::StreamNotice {
            text: String::new(),
        })
        .await
        .unwrap();

        assert_eq!(
            app.messages.len(),
            baseline + 1,
            "two empty notices should collapse to a single block"
        );
        match app.messages.last() {
            Some(MessageBlock::System { text }) => {
                assert_eq!(text, &format_with_count("", 2));
            }
            other => panic!("expected System block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn event_stream_notice_coalesces_text_that_looks_like_count_suffix() {
        // Regression for a latent footgun (Medium finding M2): if a notice
        // text happens to END with `" (×N)"` as part of its literal content
        // (not as a counter), repeated identical notices must still
        // coalesce. The literal-equality early-out in the handler catches
        // this case before the parser can interpret the trailing `(×N)`
        // as a counter, which would otherwise drop into `format!`'s
        // weirder branches and produce `"… (×N) (×M)"` displays.
        let mut app = make_test_app();
        let baseline = app.messages.len();
        let trailing_count_text = "Multiplied by (×100)".to_string();

        app.handle_event(AppEvent::StreamNotice {
            text: trailing_count_text.clone(),
        })
        .await
        .unwrap();
        app.handle_event(AppEvent::StreamNotice {
            text: trailing_count_text.clone(),
        })
        .await
        .unwrap();

        assert_eq!(
            app.messages.len(),
            baseline + 1,
            "literal text ending in `(×N)` must still coalesce on repeat"
        );
        match app.messages.last() {
            Some(MessageBlock::System { text }) => {
                // Display is `"Multiplied by (×100) (×2)"` — the original
                // literal, then our actual repeat counter.
                assert_eq!(text, &format_with_count(&trailing_count_text, 2));
            }
            other => panic!("expected System block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn event_stream_notice_count_saturates_at_usize_max() {
        // Defense-in-depth: a malformed/synthetic prior block whose count
        // is already `usize::MAX` must not panic on an additional fold.
        // saturating_add freezes the counter and the next event becomes a
        // silent no-op (the displayed text doesn't change).
        let mut app = make_test_app();
        let already_max = format_with_count("X", usize::MAX);
        app.messages.push(MessageBlock::System {
            text: already_max.clone(),
        });
        let baseline = app.messages.len();

        app.handle_event(AppEvent::StreamNotice { text: "X".into() })
            .await
            .unwrap();

        assert_eq!(
            app.messages.len(),
            baseline,
            "saturated fold must not push a new block"
        );
        match app.messages.last() {
            Some(MessageBlock::System { text }) => {
                assert_eq!(
                    text, &already_max,
                    "saturated count display should not change"
                );
            }
            other => panic!("expected System block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn event_compact_error_sets_auto_compact_failed() {
        let mut app = make_test_app();
        app.is_loading = true;
        assert!(!app.auto_compact_failed);

        app.handle_event(AppEvent::CompactError {
            error: "boom".into(),
        })
        .await
        .unwrap();

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
        })
        .await
        .unwrap();

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
        assert!(
            !state.auto_scroll,
            "scrolling up should disable auto_scroll"
        );
        state.scroll_down(1);
        assert_eq!(state.scroll_offset, 400);
        assert!(
            state.auto_scroll,
            "returning to bottom should re-enable auto_scroll"
        );
    }

    #[tokio::test]
    async fn event_lsp_restart_needed_does_not_panic() {
        let mut app = make_test_app();
        app.handle_event(AppEvent::LspRestartNeeded {
            lang: crate::lsp::Language::Rust,
        })
        .await
        .unwrap();
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
        assert!(
            state.auto_scroll,
            "page down to bottom should re-enable auto_scroll"
        );
    }

    #[tokio::test]
    async fn event_tick_shows_restarting_lsp_in_sidebar() {
        let mut app = make_test_app();
        {
            let mut map = app.lsp_status_cache.lock().unwrap();
            // Clear any entries seeded during App::new so we get exactly one.
            map.clear();
            map.insert(
                crate::lsp::Language::Rust,
                crate::lsp::LspStatusEntry {
                    binary: "rust-analyzer".into(),
                    state: crate::lsp::LspServerState::Restarting,
                    active_progress: 0,
                    progress_message: None,
                    updated_at: std::time::Instant::now(),
                    restart_attempts: 1,
                    next_restart_at: Some(
                        std::time::Instant::now() + std::time::Duration::from_secs(3),
                    ),
                    ready_since: None,
                },
            );
        }
        app.handle_event(AppEvent::Tick).await.unwrap();
        assert_eq!(app.sidebar_state.lsp_servers.len(), 1);
        assert_eq!(
            app.sidebar_state.lsp_servers[0].state,
            crate::lsp::LspServerState::Restarting
        );
        assert!(app.sidebar_state.lsp_servers[0].next_restart_at.is_some());
    }

    #[tokio::test]
    async fn llm_usage_update_sets_prompt_tokens_without_session_storage() {
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
}
