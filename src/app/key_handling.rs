use super::*;

impl App {
    /// Copy text to the system clipboard.
    /// Tries pbcopy (macOS), xclip (Linux), then falls back to OSC 52.
    pub(super) fn copy_to_clipboard(&mut self, text: &str) {
        use std::{
            io::Write as _,
            process::{Command, Stdio},
        };

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
        let write_result =
            std::io::Write::write_fmt(&mut stdout, format_args!("\x1b]52;c;{encoded}\x07"));
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

        // Intercept keys for modal prompts and overlays
        if self.pending_permission.is_some() {
            return self.handle_permission_key(key);
        }
        if self.pending_question.is_some() {
            return self.handle_question_key(key);
        }
        if self.pending_agents_update.is_some() {
            return self.handle_agents_update_key(key);
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
                    if let Some(model_ref) = self.model_picker.selected_ref().map(|s| s.to_string())
                    {
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
                self.autocomplete_state
                    .update_with_files(&current_text, &file_index);
            }
        }
        Ok(())
    }

    /// Handle keystrokes when a permission prompt is active.
    fn handle_permission_key(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                if let Some(perm) = self.pending_permission.take() {
                    let _ = perm.response_tx.send(PermissionReply::AllowOnce);
                    self.remove_last_permission_block();
                    self.status_line_state.set_activity(Activity::Thinking);
                    self.message_area_state.scroll_to_bottom();
                }
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
            }
            (KeyCode::Char('a'), _) | (KeyCode::Char('A'), _) => {
                if let Some(perm) = self.pending_permission.take() {
                    let tool_str = perm.tool_name.as_str().to_string();
                    let is_mcp = perm.summary.starts_with("MCP: ");
                    let _ = perm.response_tx.send(PermissionReply::AllowAlways);
                    self.remove_last_permission_block();
                    self.status_line_state.set_activity(Activity::Thinking);
                    self.message_area_state.scroll_to_bottom();

                    if !is_mcp {
                        self.persist_tool_grant(&tool_str);
                    }
                }
            }
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.cancel_stream();
            }
            _ => {}
        }
        Ok(())
    }

    /// Handle keystrokes when a question prompt is active.
    fn handle_question_key(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                if let Some(q) = self.pending_question.take() {
                    let _ = q.response_tx.send("User cancelled.".to_string());
                }
                self.cancel_stream();
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
                    self.mark_question_answered(&display_answer);
                    self.status_line_state.set_activity(Activity::Thinking);
                    self.message_area_state.scroll_to_bottom();
                }
            }
            (KeyCode::Esc, _) => {
                if let Some(q) = self.pending_question.take() {
                    let _ = q.response_tx.send("User declined to answer.".to_string());
                    self.mark_question_answered("(skipped)");
                    self.status_line_state.set_activity(Activity::Thinking);
                    self.message_area_state.scroll_to_bottom();
                }
            }
            (KeyCode::Char(c @ '1'..='9'), _) => {
                if let Some(q) = self.pending_question.as_mut() {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < q.options.len() {
                        q.selected = Some(idx);
                        self.sync_question_block();
                    }
                }
            }
            (KeyCode::Up, _) => {
                if let Some(q) = self.pending_question.as_mut()
                    && let Some(sel) = q.selected
                    && sel > 0
                {
                    q.selected = Some(sel - 1);
                    self.sync_question_block();
                }
            }
            (KeyCode::Down, _) => {
                if let Some(q) = self.pending_question.as_mut()
                    && let Some(sel) = q.selected
                    && sel + 1 < q.options.len()
                {
                    q.selected = Some(sel + 1);
                    self.sync_question_block();
                }
            }
            (KeyCode::Tab, _) => {
                if let Some(q) = self.pending_question.as_mut() {
                    if q.options.is_empty() {
                        // No options — already in free-text mode, ignore
                    } else if q.selected.is_some() {
                        q.selected = None;
                        self.sync_question_block();
                    } else {
                        q.selected = Some(0);
                        self.sync_question_block();
                    }
                }
            }
            (KeyCode::Char(c), _) => {
                if let Some(q) = self.pending_question.as_mut()
                    && q.selected.is_none()
                {
                    q.free_text.push(c);
                    self.sync_question_block();
                }
            }
            (KeyCode::Backspace, _) => {
                if let Some(q) = self.pending_question.as_mut()
                    && q.selected.is_none()
                {
                    q.free_text.pop();
                    self.sync_question_block();
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Handle keystrokes when an AGENTS.md update is pending approval.
    fn handle_agents_update_key(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                let Some(content) = self.pending_agents_update.take() else {
                    return Ok(());
                };
                let agents_path = self.project.root.join("AGENTS.md");
                match std::fs::write(&agents_path, &content) {
                    Ok(_) => {
                        if let Some(existing) =
                            self.agents_files.iter_mut().find(|f| f.path == agents_path)
                        {
                            existing.content = content;
                        } else {
                            self.agents_files.insert(
                                0,
                                crate::config::AgentsFile {
                                    path: agents_path.clone(),
                                    content,
                                },
                            );
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
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{has_system_message, make_test_app};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn press_ctrl(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn make_pending_permission(app: &mut App) -> tokio::sync::oneshot::Receiver<PermissionReply> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.pending_permission = Some(PendingPermission {
            tool_name: ToolName::Bash,
            summary: "test command".into(),
            response_tx: tx,
        });
        rx
    }

    fn make_pending_question(
        app: &mut App,
        options: Vec<String>,
    ) -> tokio::sync::oneshot::Receiver<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let has_options = !options.is_empty();
        app.pending_question = Some(PendingQuestion {
            call_id: "call-1".into(),
            question: "Pick one".into(),
            options,
            selected: if has_options { Some(0) } else { None },
            free_text: String::new(),
            response_tx: tx,
        });
        app.messages.push(MessageBlock::Question {
            question: "Pick one".into(),
            options: vec![],
            selected: if has_options { Some(0) } else { None },
            free_text: String::new(),
            answered: None,
        });
        rx
    }

    // ─── Permission key tests ───

    #[tokio::test]
    async fn permission_y_sends_allow_once() {
        let mut app = make_test_app();
        let mut rx = make_pending_permission(&mut app);
        app.handle_key(press(KeyCode::Char('y'))).await.unwrap();
        assert!(app.pending_permission.is_none());
        assert!(matches!(rx.try_recv().unwrap(), PermissionReply::AllowOnce));
    }

    #[tokio::test]
    async fn permission_n_sends_deny() {
        let mut app = make_test_app();
        let mut rx = make_pending_permission(&mut app);
        app.handle_key(press(KeyCode::Char('n'))).await.unwrap();
        assert!(app.pending_permission.is_none());
        assert!(matches!(rx.try_recv().unwrap(), PermissionReply::Deny));
        assert!(has_system_message(&app, "denied"));
    }

    #[tokio::test]
    async fn permission_a_sends_allow_always() {
        let mut app = make_test_app();
        let mut rx = make_pending_permission(&mut app);
        app.handle_key(press(KeyCode::Char('a'))).await.unwrap();
        assert!(app.pending_permission.is_none());
        assert!(matches!(
            rx.try_recv().unwrap(),
            PermissionReply::AllowAlways
        ));
    }

    #[tokio::test]
    async fn permission_esc_sends_deny() {
        let mut app = make_test_app();
        let mut rx = make_pending_permission(&mut app);
        app.handle_key(press(KeyCode::Esc)).await.unwrap();
        assert!(matches!(rx.try_recv().unwrap(), PermissionReply::Deny));
    }

    #[tokio::test]
    async fn permission_ctrl_c_cancels_stream() {
        let mut app = make_test_app();
        let _rx = make_pending_permission(&mut app);
        app.streaming_active = true;
        app.handle_key(press_ctrl('c')).await.unwrap();
        assert!(app.pending_permission.is_none());
        assert!(has_system_message(&app, "cancelled"));
    }

    #[tokio::test]
    async fn permission_ignores_other_keys() {
        let mut app = make_test_app();
        let _rx = make_pending_permission(&mut app);
        app.handle_key(press(KeyCode::Char('x'))).await.unwrap();
        // Permission should still be pending
        assert!(app.pending_permission.is_some());
    }

    // ─── Question key tests ───

    #[tokio::test]
    async fn question_enter_submits_selected_option() {
        let mut app = make_test_app();
        let mut rx = make_pending_question(&mut app, vec!["red".into(), "blue".into()]);
        app.handle_key(press(KeyCode::Enter)).await.unwrap();
        assert!(app.pending_question.is_none());
        assert_eq!(rx.try_recv().unwrap(), "red"); // first option selected by default
    }

    #[tokio::test]
    async fn question_number_selects_option() {
        let mut app = make_test_app();
        let mut rx =
            make_pending_question(&mut app, vec!["red".into(), "blue".into(), "green".into()]);
        // Press '2' to select "blue", then Enter
        app.handle_key(press(KeyCode::Char('2'))).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().selected, Some(1));
        app.handle_key(press(KeyCode::Enter)).await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), "blue");
    }

    #[tokio::test]
    async fn question_esc_skips() {
        let mut app = make_test_app();
        let mut rx = make_pending_question(&mut app, vec!["red".into()]);
        app.handle_key(press(KeyCode::Esc)).await.unwrap();
        assert!(app.pending_question.is_none());
        assert_eq!(rx.try_recv().unwrap(), "User declined to answer.");
    }

    #[tokio::test]
    async fn question_up_down_navigates() {
        let mut app = make_test_app();
        let _rx = make_pending_question(&mut app, vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(app.pending_question.as_ref().unwrap().selected, Some(0));

        app.handle_key(press(KeyCode::Down)).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().selected, Some(1));

        app.handle_key(press(KeyCode::Down)).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().selected, Some(2));

        // Down at end stays at end
        app.handle_key(press(KeyCode::Down)).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().selected, Some(2));

        app.handle_key(press(KeyCode::Up)).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().selected, Some(1));
    }

    #[tokio::test]
    async fn question_tab_toggles_free_text() {
        let mut app = make_test_app();
        let _rx = make_pending_question(&mut app, vec!["a".into()]);
        assert!(app.pending_question.as_ref().unwrap().selected.is_some());

        // Tab → free text mode
        app.handle_key(press(KeyCode::Tab)).await.unwrap();
        assert!(app.pending_question.as_ref().unwrap().selected.is_none());

        // Tab → back to options
        app.handle_key(press(KeyCode::Tab)).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().selected, Some(0));
    }

    #[tokio::test]
    async fn question_free_text_input() {
        let mut app = make_test_app();
        let mut rx = make_pending_question(&mut app, vec!["a".into()]);

        // Switch to free text
        app.handle_key(press(KeyCode::Tab)).await.unwrap();
        app.handle_key(press(KeyCode::Char('h'))).await.unwrap();
        app.handle_key(press(KeyCode::Char('i'))).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().free_text, "hi");

        // Backspace
        app.handle_key(press(KeyCode::Backspace)).await.unwrap();
        assert_eq!(app.pending_question.as_ref().unwrap().free_text, "h");

        // Submit free text
        app.handle_key(press(KeyCode::Char('i'))).await.unwrap();
        app.handle_key(press(KeyCode::Enter)).await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), "hi");
    }

    // ─── Agents update key tests ───

    #[tokio::test]
    async fn agents_update_n_discards() {
        let mut app = make_test_app();
        app.pending_agents_update = Some("proposed content".into());
        app.handle_key(press(KeyCode::Char('n'))).await.unwrap();
        assert!(app.pending_agents_update.is_none());
        assert!(has_system_message(&app, "AGENTS.md update discarded"));
    }

    #[tokio::test]
    async fn agents_update_esc_discards() {
        let mut app = make_test_app();
        app.pending_agents_update = Some("proposed content".into());
        app.handle_key(press(KeyCode::Esc)).await.unwrap();
        assert!(app.pending_agents_update.is_none());
    }

    // ─── Normal key handling tests ───

    #[tokio::test]
    async fn ctrl_c_when_streaming_cancels() {
        let mut app = make_test_app();
        app.streaming_active = true;
        app.is_loading = true;

        app.handle_key(press_ctrl('c')).await.unwrap();

        assert!(!app.streaming_active);
        assert!(!app.should_quit);
    }

    #[tokio::test]
    async fn ctrl_c_when_idle_quits() {
        let mut app = make_test_app();
        assert!(!app.should_quit);

        app.handle_key(press_ctrl('c')).await.unwrap();

        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn ctrl_b_toggles_sidebar() {
        let mut app = make_test_app();
        assert!(app.sidebar_override.is_none());

        app.handle_key(press_ctrl('b')).await.unwrap();
        assert_eq!(app.sidebar_override, Some(false));

        app.handle_key(press_ctrl('b')).await.unwrap();
        assert_eq!(app.sidebar_override, Some(true));

        app.handle_key(press_ctrl('b')).await.unwrap();
        assert!(app.sidebar_override.is_none());
    }

    #[tokio::test]
    async fn key_release_ignored() {
        let mut app = make_test_app();
        let release = KeyEvent {
            code: KeyCode::Char('y'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        // Should be a no-op
        let _rx = make_pending_permission(&mut app);
        app.handle_key(release).await.unwrap();
        // Permission should still be pending — release was ignored
        assert!(app.pending_permission.is_some());
    }
}
