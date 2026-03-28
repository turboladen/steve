use super::*;

impl App {
    /// Update the sidebar state from current app state.
    /// Note: token counters are NOT synced here — they are updated live by
    /// `LlmUsageUpdate` (accumulate per-call) and authoritatively by
    /// `sync_sidebar_tokens()` after `add_usage()` on `LlmFinish`.
    pub(super) fn update_sidebar(&mut self) {
        /// Map priority to a sort key (lower = higher priority).
        fn priority_sort_key(p: Priority) -> u8 {
            match p {
                Priority::High => 0,
                Priority::Medium => 1,
                Priority::Low => 2,
            }
        }

        if let Some(session) = &self.current_session {
            self.sidebar_state.session_title = session.title.clone();
        }
        if let Some(model) = &self.current_model {
            self.sidebar_state.model_name = model.clone();
        }
        // Calculate session cost if model has pricing
        self.sidebar_state.session_cost = None;
        if let (Some(model_ref), Some(registry), Some(session)) = (
            &self.current_model,
            &self.provider_registry,
            &self.current_session,
        ) {
            if let Ok(resolved) = registry.resolve_model(model_ref) {
                self.sidebar_state.session_cost = resolved.session_cost(
                    session.token_usage.prompt_tokens,
                    session.token_usage.completion_tokens,
                );
            }
        }
        // Sync task list for sidebar: open/in_progress tasks + session-closed tasks
        self.sidebar_state.tasks = self
            .task_store
            .list_tasks()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| {
                t.status != TaskStatus::Done
                    || self.sidebar_state.session_closed_task_ids.contains(&t.id)
            })
            .map(|t| SidebarTask::from(t))
            .collect();
        // Sort: open/in_progress first (by priority High→Low), then done at bottom
        self.sidebar_state.tasks.sort_by(|a, b| {
            let a_done = a.status == TaskStatus::Done;
            let b_done = b.status == TaskStatus::Done;
            a_done.cmp(&b_done).then_with(|| {
                // Within same done/not-done group, sort by priority (High < Medium < Low)
                priority_sort_key(a.priority).cmp(&priority_sort_key(b.priority))
            })
        });
        self.sidebar_state.tasks.truncate(MAX_SIDEBAR_TASKS);

        // Sync status line state
        if let Some(model) = &self.current_model {
            self.status_line_state.model_name = model.clone();
        }
        if let Some(session) = &self.current_session {
            self.status_line_state.total_tokens = session.token_usage.total_tokens;
        }
        self.status_line_state.last_prompt_tokens = self.last_prompt_tokens;
    }

    /// Eagerly resolve the current model's context window for border color display.
    /// Call after any change to `current_model` (startup, `/model`, session switch).
    pub(super) fn sync_context_window(&mut self) {
        if let (Some(model_ref), Some(registry)) = (&self.current_model, &self.provider_registry) {
            if let Ok(resolved) = registry.resolve_model(model_ref) {
                self.status_line_state.context_window = resolved.config.context_window as u64;
            }
        }
    }

    /// Sync sidebar token counters from the authoritative session data.
    /// Call after `add_usage()` (LlmFinish) or session reset (/new).
    pub(super) fn sync_sidebar_tokens(&mut self) {
        if let Some(session) = &self.current_session {
            self.sidebar_state.prompt_tokens = session.token_usage.prompt_tokens;
            self.sidebar_state.completion_tokens = session.token_usage.completion_tokens;
            self.sidebar_state.total_tokens = session.token_usage.total_tokens;
        } else {
            self.sidebar_state.prompt_tokens = 0;
            self.sidebar_state.completion_tokens = 0;
            self.sidebar_state.total_tokens = 0;
        }
    }

    /// Run all diagnostic checks against current app state.
    /// Called at discrete sync points (LlmFinish, CompactFinish, /new, switch_to_session)
    /// and when the /diagnostics overlay is opened — not per-frame.
    pub(super) fn collect_diagnostics(&self) -> Vec<crate::diagnostics::DiagnosticCheck> {
        let (cache_hits, cache_misses) = self.tool_cache.lock().unwrap().cache_stats();
        let lsp_servers: Vec<(&str, bool)> = self
            .sidebar_state
            .lsp_servers
            .iter()
            .map(|s| (s.binary.as_str(), s.running))
            .collect();
        let system_prompt_len = self.build_system_prompt().map(|s| s.len()).unwrap_or(0);
        let total_tokens = self
            .current_session
            .as_ref()
            .map(|s| s.token_usage.total_tokens)
            .unwrap_or(0);
        let combined_agents = self.combined_agents_content();

        // Gather MCP server info from sidebar state (already populated via McpStatus event)
        let mcp_configured_ids: Vec<String> = self.config.mcp_servers.keys().cloned().collect();
        let mcp_configured: Vec<&str> = mcp_configured_ids.iter().map(|s| s.as_str()).collect();
        let mcp_connected: Vec<(&str, usize, usize, usize)> = self
            .sidebar_state
            .mcp_servers
            .iter()
            .filter(|s| s.connected)
            .map(|s| {
                (
                    s.server_id.as_str(),
                    s.tool_count,
                    s.resource_count,
                    s.prompt_count,
                )
            })
            .collect();

        let input = crate::diagnostics::DiagnosticInput {
            agents_md: combined_agents.as_deref(),
            system_prompt_len,
            config: &self.config,
            lsp_servers: &lsp_servers,
            total_tokens,
            exchange_count: self.exchange_count,
            cache_hits,
            cache_misses,
            compaction_count: self.compaction_count,
            session_cost: self.sidebar_state.session_cost,
            mcp_configured: &mcp_configured,
            mcp_connected: &mcp_connected,
        };
        crate::diagnostics::run_diagnostics(&input)
    }

    /// Refresh diagnostics summary for the sidebar indicator.
    pub(super) fn sync_diagnostics(&mut self) {
        let checks = self.collect_diagnostics();
        self.sidebar_state.diagnostics_summary = crate::diagnostics::summarize(&checks);
    }

    /// Refresh git information in the sidebar state.
    pub(super) fn refresh_git_info(&mut self) {
        use crate::project::{git_branch, git_is_dirty, git_repo_name};
        self.sidebar_state.git_branch = git_branch(&self.project.root);
        self.sidebar_state.git_dirty = git_is_dirty(&self.project.root);
        self.sidebar_state.git_repo_name = git_repo_name(&self.project.root);
    }

    /// Sync the permission engine rules with the current agent mode.
    pub(super) fn sync_permission_mode(&self) {
        use crate::{
            permission::{PermissionProfile, profile_build_rules, profile_plan_rules},
            ui::input::AgentMode,
        };

        let profile = self
            .config
            .permission_profile
            .unwrap_or(PermissionProfile::Standard);
        let allow_overrides: Vec<ToolName> = self
            .config
            .allow_tools
            .iter()
            .filter_map(|s| s.parse::<ToolName>().ok())
            .collect();

        let path_rules = &self.config.permission_rules;
        let rules = match self.input.mode {
            AgentMode::Build => profile_build_rules(profile, &allow_overrides, path_rules),
            AgentMode::Plan => profile_plan_rules(profile, &allow_overrides, path_rules),
        };

        // Spawn a task to update the engine since it requires async lock
        let engine = self.permission_engine.clone();
        let is_plan = self.input.mode == AgentMode::Plan;
        tokio::spawn(async move {
            let mut engine = engine.lock().await;
            engine.set_rules(rules);
            engine.set_plan_mode(is_plan);
        });
    }

    /// Persist a tool grant to the project config and update in-memory config.
    pub(super) fn persist_tool_grant(&mut self, tool_name: &str) {
        // Update in-memory config
        if !self.config.allow_tools.contains(&tool_name.to_string()) {
            self.config.allow_tools.push(tool_name.to_string());
            // Re-sync permission rules with updated config
            self.sync_permission_mode();
        }

        // Persist to disk (fire-and-forget — don't block the UI)
        let project_root = self.project.root.clone();
        let tool = tool_name.to_string();
        std::thread::spawn(move || {
            if let Err(e) = crate::config::persist_allow_tool(&project_root, &tool) {
                tracing::warn!("failed to persist tool grant: {e}");
            }
        });
    }

    /// Check if the session is approaching the context window limit and
    /// auto-compact should be triggered.
    pub(super) fn check_context_warning(&mut self) {
        if self.context_warned {
            return;
        }
        let context_window = self.status_line_state.context_window;
        let prompt_tokens = self.status_line_state.last_prompt_tokens;
        if context_window == 0 {
            return;
        }
        let threshold = (context_window as f64 * 0.60) as u64;
        if prompt_tokens >= threshold {
            self.context_warned = true;
            let pct = self.status_line_state.context_usage_pct();
            self.messages.push(MessageBlock::System {
                text: format!(
                    "Context window {}% full ({}/{}). Consider /compact to free space.",
                    pct,
                    crate::ui::status_line::format_tokens(prompt_tokens),
                    crate::ui::status_line::format_tokens(context_window),
                ),
            });
            self.message_area_state.scroll_to_bottom();
        }
    }

    pub(super) fn should_auto_compact(&self) -> bool {
        if !self.config.auto_compact {
            return false;
        }

        if self.auto_compact_failed {
            return false;
        }

        // Need at least a few messages to make compaction worthwhile
        if self.stored_messages.len() < 4 {
            return false;
        }

        if self.current_session.is_none() {
            return false;
        }

        let Some(model_ref) = &self.current_model else {
            return false;
        };

        let Some(registry) = &self.provider_registry else {
            return false;
        };

        let Ok(resolved) = registry.resolve_model(model_ref) else {
            return false;
        };

        let context_window = resolved.config.context_window as u64;
        let threshold = (context_window as f64 * 0.80) as u64;

        self.last_prompt_tokens >= threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{make_test_app, make_test_registry};

    #[test]
    fn check_context_warning_fires_at_60_pct() {
        let mut app = make_test_app();
        app.context_warned = false;
        app.status_line_state.context_window = 128_000;
        app.last_prompt_tokens = 80_000; // ~62%
        app.status_line_state.last_prompt_tokens = 80_000;
        app.check_context_warning();
        assert!(app.context_warned);
        assert!(app.messages.iter().any(|m| {
            matches!(m, MessageBlock::System { text } if text.contains("Context window"))
        }));
    }

    #[test]
    fn check_context_warning_only_fires_once() {
        let mut app = make_test_app();
        app.context_warned = false;
        app.status_line_state.context_window = 128_000;
        app.last_prompt_tokens = 80_000;
        app.status_line_state.last_prompt_tokens = 80_000;
        app.check_context_warning();
        let msg_count = app.messages.len();
        app.check_context_warning(); // second call
        assert_eq!(app.messages.len(), msg_count); // no new message
    }

    #[test]
    fn check_context_warning_does_not_fire_below_threshold() {
        let mut app = make_test_app();
        app.status_line_state.context_window = 128_000;
        app.last_prompt_tokens = 50_000; // ~39%
        app.status_line_state.last_prompt_tokens = 50_000;
        app.check_context_warning();
        assert!(!app.context_warned);
    }

    #[test]
    fn sync_context_window_sets_from_registry() {
        let mut app = make_test_app();
        assert_eq!(app.status_line_state.context_window, 0);

        app.provider_registry = Some(make_test_registry(128_000));
        app.current_model = Some("test/test-model".to_string());
        app.sync_context_window();

        assert_eq!(app.status_line_state.context_window, 128_000);
    }

    #[test]
    fn sync_context_window_noop_without_registry() {
        let mut app = make_test_app();
        app.current_model = Some("test/test-model".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 0);
    }

    #[test]
    fn sync_context_window_noop_without_model() {
        let mut app = make_test_app();
        app.provider_registry = Some(make_test_registry(128_000));
        app.current_model = None;
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 0);
    }

    #[test]
    fn sync_context_window_invalid_model_preserves_previous() {
        let mut app = make_test_app();
        app.provider_registry = Some(make_test_registry(128_000));
        app.current_model = Some("test/test-model".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 128_000);

        // Switch to an invalid model — previous value should be preserved
        app.current_model = Some("nonexistent/model".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 128_000);
    }

    #[test]
    fn sync_context_window_updates_on_model_change() {
        let mut app = make_test_app();
        let mut models = std::collections::HashMap::new();
        models.insert(
            "small".to_string(),
            crate::config::types::ModelConfig {
                id: "small".to_string(),
                name: "Small".to_string(),
                context_window: 32_000,
                max_output_tokens: None,
                cost: None,
                capabilities: crate::config::types::ModelCapabilities {
                    tool_call: true,
                    reasoning: false,
                },
            },
        );
        models.insert(
            "large".to_string(),
            crate::config::types::ModelConfig {
                id: "large".to_string(),
                name: "Large".to_string(),
                context_window: 200_000,
                max_output_tokens: None,
                cost: None,
                capabilities: crate::config::types::ModelCapabilities {
                    tool_call: true,
                    reasoning: false,
                },
            },
        );
        let provider_config = crate::config::types::ProviderConfig {
            base_url: "https://api.test.com/v1".to_string(),
            api_key_env: "TEST_KEY".to_string(),
            models,
        };
        let client = crate::provider::client::LlmClient::new("https://api.test.com/v1", "fake");
        app.provider_registry = Some(crate::provider::ProviderRegistry::from_entries(vec![(
            "test".to_string(),
            provider_config,
            client,
        )]));

        app.current_model = Some("test/small".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 32_000);

        app.current_model = Some("test/large".to_string());
        app.sync_context_window();
        assert_eq!(app.status_line_state.context_window, 200_000);
    }
}
