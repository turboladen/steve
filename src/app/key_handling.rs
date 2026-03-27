use super::*;

impl App {
    /// Copy text to the system clipboard.
    /// Tries pbcopy (macOS), xclip (Linux), then falls back to OSC 52.
    pub(super) fn copy_to_clipboard(&mut self, text: &str) {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        // Try pbcopy (macOS)
        let ok = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    stdin.write_all(text.as_bytes())?;
                }
                child.wait()
            })
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            self.selection_state.copied_flash = Some(std::time::Instant::now());
            return;
        }

        // Try xclip (Linux)
        let ok = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    stdin.write_all(text.as_bytes())?;
                }
                child.wait()
            })
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            self.selection_state.copied_flash = Some(std::time::Instant::now());
            return;
        }

        // Fallback: OSC 52
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(text);
        let mut stdout = std::io::stdout();
        let write_result = std::io::Write::write_fmt(
            &mut stdout,
            format_args!("\x1b]52;c;{encoded}\x07"),
        );
        let result = write_result.and_then(|_| std::io::Write::flush(&mut stdout));
        if result.is_ok() {
            self.selection_state.copied_flash = Some(std::time::Instant::now());
        } else {
            tracing::error!("failed to copy to clipboard via any method");
        }
    }

    pub(super) async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // Only process key presses — ignore Release/Repeat events from enhanced keyboard protocol
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        // If there's a pending permission prompt, intercept keystrokes
        if self.pending_permission.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::AllowOnce);
                        self.remove_last_permission_block();
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Char('n'), _) | (KeyCode::Char('N'), _) | (KeyCode::Esc, _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let _ = perm.response_tx.send(PermissionReply::Deny);
                        self.remove_last_permission_block();
                        self.messages.push(MessageBlock::System {
                            text: format!("\u{2717} denied: {}", perm.tool_name),
                        });
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Char('a'), _) | (KeyCode::Char('A'), _) => {
                    if let Some(perm) = self.pending_permission.take() {
                        let tool_str = perm.tool_name.as_str().to_string();
                        // Check if this is an MCP tool (placeholder ToolName::Bash
                        // with MCP summary). MCP grants are session-only — don't
                        // persist to config since MCP tool names are runtime-dynamic.
                        let is_mcp = perm.summary.starts_with("MCP: ");
                        let _ = perm.response_tx.send(PermissionReply::AllowAlways);
                        self.remove_last_permission_block();
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();

                        if !is_mcp {
                            // Persist the grant to project config so it survives restarts
                            self.persist_tool_grant(&tool_str);
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    // Cancel the stream (which will drop the permission request)
                    self.cancel_stream();
                    return Ok(());
                }
                _ => {
                    // Ignore other keys while permission prompt is active
                    return Ok(());
                }
            }
        }

        // If there's a pending question prompt, intercept keystrokes
        if self.pending_question.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    // Cancel the stream entirely
                    if let Some(q) = self.pending_question.take() {
                        let _ = q.response_tx.send("User cancelled.".to_string());
                    }
                    self.cancel_stream();
                    return Ok(());
                }
                (KeyCode::Enter, _) => {
                    if let Some(q) = self.pending_question.take() {
                        let answer = if let Some(idx) = q.selected {
                            q.options.get(idx).cloned().unwrap_or_default()
                        } else if q.free_text.is_empty() {
                            "User declined to answer.".to_string()
                        } else {
                            q.free_text.clone()
                        };
                        let display_answer = answer.clone();
                        let _ = q.response_tx.send(answer);
                        // Mark the question block as answered
                        self.mark_question_answered(&display_answer);
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Esc, _) => {
                    if let Some(q) = self.pending_question.take() {
                        let _ = q.response_tx.send("User declined to answer.".to_string());
                        self.mark_question_answered("(skipped)");
                        self.status_line_state.set_activity(Activity::Thinking);
                        self.message_area_state.scroll_to_bottom();
                    }
                    return Ok(());
                }
                (KeyCode::Char(c @ '1'..='9'), _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        let idx = (c as usize) - ('1' as usize);
                        if idx < q.options.len() {
                            q.selected = Some(idx);
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Up, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if let Some(sel) = q.selected {
                            if sel > 0 {
                                q.selected = Some(sel - 1);
                                self.sync_question_block();
                            }
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Down, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if let Some(sel) = q.selected {
                            if sel + 1 < q.options.len() {
                                q.selected = Some(sel + 1);
                                self.sync_question_block();
                            }
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Tab, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if q.options.is_empty() {
                            // No options — already in free-text mode, ignore
                        } else if q.selected.is_some() {
                            // Switch to free-text mode
                            q.selected = None;
                            self.sync_question_block();
                        } else {
                            // Switch back to options mode
                            q.selected = Some(0);
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Char(c), _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if q.selected.is_none() {
                            // Free-text mode
                            q.free_text.push(c);
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                (KeyCode::Backspace, _) => {
                    if let Some(q) = self.pending_question.as_mut() {
                        if q.selected.is_none() {
                            q.free_text.pop();
                            self.sync_question_block();
                        }
                    }
                    return Ok(());
                }
                _ => {
                    // Swallow other keys
                    return Ok(());
                }
            }
        }

        // If there's a pending AGENTS.md update awaiting approval, intercept y/n
        if self.pending_agents_update.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                    let content = self.pending_agents_update.take().unwrap();
                    let agents_path = self.project.root.join("AGENTS.md");
                    match std::fs::write(&agents_path, &content) {
                        Ok(_) => {
                            // Update or insert root-level entry in the chain
                            if let Some(existing) = self.agents_files.iter_mut().find(|f| f.path == agents_path) {
                                existing.content = content;
                            } else {
                                self.agents_files.insert(0, crate::config::AgentsFile {
                                    path: agents_path.clone(),
                                    content,
                                });
                            }
                            self.messages.push(MessageBlock::System {
                                text: format!("AGENTS.md updated at {}", agents_path.display()),
                            });
                        }
                        Err(e) => {
                            self.messages.push(MessageBlock::Error {
                                text: format!("Failed to write AGENTS.md: {e}"),
                            });
                        }
                    }
                    self.message_area_state.scroll_to_bottom();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.discard_pending_agents_update();
                }
                (KeyCode::Char('n'), _) | (KeyCode::Char('N'), _) | (KeyCode::Esc, _) => {
                    self.discard_pending_agents_update();
                }
                _ => {
                    // Ignore other keys
                }
            }
            return Ok(());
        }

        // If the model picker overlay is open, intercept keystrokes
        if self.model_picker.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.model_picker.close();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.model_picker.close();
                    // Also cancel any active stream (unlikely but possible)
                    if self.is_loading || self.streaming_active {
                        self.cancel_stream();
                    }
                }
                (KeyCode::Up, _) => {
                    self.model_picker.prev();
                }
                (KeyCode::Down, _) => {
                    self.model_picker.next();
                }
                (KeyCode::Enter, _) => {
                    if let Some(model_ref) = self.model_picker.selected_ref().map(|s| s.to_string()) {
                        self.model_picker.close();
                        self.handle_input(format!("/model {model_ref}")).await?;
                    }
                }
                (KeyCode::Backspace, _) => {
                    self.model_picker.backspace();
                }
                (KeyCode::Char(c), _) => {
                    self.model_picker.type_char(c);
                }
                _ => {}
            }
            return Ok(());
        }

        // If the session picker overlay is open, intercept keystrokes
        if self.session_picker.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.session_picker.close();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.session_picker.close();
                    if self.is_loading || self.streaming_active {
                        self.cancel_stream();
                    }
                }
                (KeyCode::Up, _) => {
                    self.session_picker.prev();
                }
                (KeyCode::Down, _) => {
                    self.session_picker.next();
                }
                (KeyCode::Enter, _) => {
                    if let Some(session) = self.session_picker.selected_session() {
                        self.session_picker.close();
                        self.switch_to_session(session).await?;
                    }
                }
                (KeyCode::Backspace, _) => {
                    self.session_picker.backspace();
                }
                (KeyCode::Char(c), _) => {
                    self.session_picker.type_char(c);
                }
                _ => {}
            }
            return Ok(());
        }

        // If the MCP overlay is open, intercept keystrokes
        if self.mcp_overlay.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.mcp_overlay.close();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.mcp_overlay.close();
                    if self.is_loading || self.streaming_active {
                        self.cancel_stream();
                    }
                }
                (KeyCode::Up, _) => {
                    self.mcp_overlay.scroll_up(1);
                }
                (KeyCode::Down, _) => {
                    self.mcp_overlay.scroll_down(1);
                }
                (KeyCode::Tab, _) | (KeyCode::Right, _) => {
                    self.mcp_overlay.next_tab();
                }
                (KeyCode::BackTab, _) | (KeyCode::Left, _) => {
                    self.mcp_overlay.prev_tab();
                }
                _ => {}
            }
            return Ok(());
        }

        // If the diagnostics overlay is open, intercept keystrokes
        if self.diagnostics_overlay.visible {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    self.diagnostics_overlay.close();
                }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.diagnostics_overlay.close();
                    if self.is_loading || self.streaming_active {
                        self.cancel_stream();
                    }
                }
                (KeyCode::Up, _) => {
                    self.diagnostics_overlay.scroll_up(1);
                }
                (KeyCode::Down, _) => {
                    self.diagnostics_overlay.scroll_down(1);
                }
                _ => {}
            }
            return Ok(());
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.is_loading || self.streaming_active {
                    self.cancel_stream();
                } else {
                    self.should_quit = true;
                }
            }
            (KeyCode::Enter, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                match self.autocomplete_state.mode {
                    AutocompleteMode::Command => {
                        if let Some(cmd_name) = self.autocomplete_state.selected_command() {
                            let cmd_name = cmd_name.to_string();
                            self.autocomplete_state.hide();
                            self.input.take_text(); // clear input
                            if !self.is_loading {
                                self.handle_input(cmd_name).await?;
                            }
                        }
                    }
                    AutocompleteMode::FileRef => {
                        if let Some(path) = self.autocomplete_state.selected_file() {
                            let path = path.to_string();
                            let current = self.input.textarea.lines().join("\n");
                            let new_text = apply_file_completion(&current, &path);
                            self.input.set_text(&new_text);
                            self.autocomplete_state.hide();
                        }
                    }
                }
            }
            (KeyCode::Tab, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                match self.autocomplete_state.mode {
                    AutocompleteMode::Command => {
                        // Tab completes the command text without executing,
                        // allowing the user to append arguments (e.g. `/mcp tools <server>`).
                        if let Some(cmd_name) = self.autocomplete_state.selected_command() {
                            let cmd_name = cmd_name.to_string();
                            self.input.set_text(&cmd_name);
                            // Re-filter so the menu stays visible with narrowed matches.
                            self.autocomplete_state.update(&cmd_name);
                        }
                    }
                    AutocompleteMode::FileRef => {
                        if let Some(path) = self.autocomplete_state.selected_file() {
                            let path = path.to_string();
                            let current = self.input.textarea.lines().join("\n");
                            let new_text = apply_file_completion(&current, &path);
                            self.input.set_text(&new_text);
                            self.autocomplete_state.hide();
                        }
                    }
                }
            }
            (KeyCode::Up, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                self.autocomplete_state.prev();
            }
            (KeyCode::Down, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                self.autocomplete_state.next();
            }
            (KeyCode::Up, KeyModifiers::NONE) => {
                self.message_area_state.scroll_up(1);
            }
            (KeyCode::Down, KeyModifiers::NONE) => {
                self.message_area_state.scroll_down(1);
            }
            (KeyCode::PageUp, KeyModifiers::NONE) => {
                let page = self.message_area_state.visible_height().max(1);
                self.message_area_state.scroll_up(page);
            }
            (KeyCode::PageDown, KeyModifiers::NONE) => {
                let page = self.message_area_state.visible_height().max(1);
                self.message_area_state.scroll_down(page);
            }
            (KeyCode::Esc, KeyModifiers::NONE) if self.autocomplete_state.visible => {
                self.autocomplete_state.hide();
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                let current_text = self.input.textarea.lines().join("\n");
                if current_text.starts_with('/') {
                    self.autocomplete_state.update(&current_text);
                } else {
                    self.input.mode = self.input.mode.toggle();
                    // Update permission rules for the new mode
                    self.sync_permission_mode();
                }
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.sidebar_override = match self.sidebar_override {
                    None => Some(false),       // auto (likely visible) -> hide
                    Some(false) => Some(true), // hidden -> show
                    Some(true) => None,        // forced visible -> auto
                };
            }
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                // Toggle paste preview overlay (only meaningful when a paste is collapsed)
                if self.input.collapsed_paste.is_some() {
                    self.input.paste_preview_visible = !self.input.paste_preview_visible;
                }
            }
            (KeyCode::Enter, KeyModifiers::SHIFT) => {
                // Shift+Enter: insert newline in textarea (forward as plain Enter)
                self.input.expand_paste();
                self.input
                    .textarea
                    .input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let text = self.input.take_text();
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    if self.is_loading {
                        self.handle_interjection(trimmed);
                    } else {
                        self.handle_input(trimmed).await?;
                    }
                }
            }
            _ => {
                // Only expand collapsed paste for keys that modify text content,
                // not navigation keys (arrows, Home, End, F-keys, etc.)
                let is_editing_key = matches!(
                    key.code,
                    KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
                );
                if is_editing_key {
                    self.input.expand_paste();
                }
                self.input.textarea.input(key);
                let current_text = self.input.textarea.lines().join("\n");
                self.ensure_file_index();
                let file_index = self.file_index.clone().unwrap_or_default();
                self.autocomplete_state.update_with_files(&current_text, &file_index);
            }
        }
        Ok(())
    }
}
