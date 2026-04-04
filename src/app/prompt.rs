use super::*;

impl App {
    /// Build API-compatible conversation history from stored messages.
    /// Excludes the last message (the current user message, passed separately).
    #[allow(deprecated)]
    pub(super) fn build_api_history(&self) -> Vec<ChatCompletionRequestMessage> {
        // All messages except the last one (which is the new user message)
        let history_messages = if self.stored_messages.len() > 1 {
            &self.stored_messages[..self.stored_messages.len() - 1]
        } else {
            return Vec::new();
        };

        history_messages
            .iter()
            .filter_map(|msg| match msg.role {
                Role::User => {
                    let text = msg.text_content();
                    if text.is_empty() {
                        return None;
                    }
                    Some(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessage {
                            content: ChatCompletionRequestUserMessageContent::Text(text),
                            name: None,
                        },
                    ))
                }
                Role::Assistant => {
                    let text = msg.text_content();
                    if text.is_empty() {
                        return None;
                    }
                    Some(ChatCompletionRequestMessage::Assistant(
                        ChatCompletionRequestAssistantMessage {
                            content: Some(ChatCompletionRequestAssistantMessageContent::Text(text)),
                            name: None,
                            audio: None,
                            tool_calls: None,
                            function_call: None,
                            refusal: None,
                        },
                    ))
                }
                Role::System => None, // System messages are handled separately
            })
            .collect()
    }

    /// Combine all loaded AGENTS.md files into a single string (for diagnostics).
    pub(super) fn combined_agents_content(&self) -> Option<String> {
        if self.agents_files.is_empty() {
            None
        } else {
            Some(
                self.agents_files
                    .iter()
                    .map(|f| f.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            )
        }
    }

    pub(super) fn build_system_prompt(&self) -> Option<String> {
        use crate::ui::input::AgentMode;

        let mut parts: Vec<String> = Vec::new();

        // Identity and environment context
        let model_name = self.current_model.as_deref().unwrap_or("unknown");
        let git_branch = self.sidebar_state.git_branch.clone();
        let mode_name = if self.input.mode == AgentMode::Plan {
            "Plan"
        } else {
            "Build"
        };

        let mut identity = format!(
            "You are Steve, a TUI AI coding agent. You help users understand, modify, and build software \
            by reading files, searching code, making edits, and running commands — all within this terminal interface.\n\n\
            ## Environment\n\
            - **Project root**: {}\n\
            - **Model**: {model_name}\n\
            - **Mode**: {mode_name}\n\
            - **Date**: {}",
            self.project.root.display(),
            chrono::Local::now().format("%A, %B %-d, %Y at %-I:%M %p")
        );
        if let Some(branch) = git_branch {
            identity.push_str(&format!("\n- **Git branch**: {branch}"));
        }

        identity.push_str("\n\n\
            ## How You Work\n\
            - You can only access files within the project root. All paths are resolved relative to it.\n\
            - **Build mode**: Read tools are auto-approved. Write tools (edit, write, patch) and bash require user permission.\n\
            - **Plan mode**: Read-only. Write tools are unavailable. Use this for analysis and planning.\n\
            - The user sees your tool calls and results in the TUI. Be concise — tool output consumes context window space.\n\
            - When context runs low, the conversation may be automatically compacted into a summary.\n\
            - Use the `memory` tool to persist important discoveries across sessions.");

        parts.push(identity);

        parts.push(TOOL_GUIDANCE.to_string());

        // Load project memory if it exists (with shared lock for safe concurrent access)
        let memory_path = self.storage.base_dir().join("memory.md");
        if let Ok(memory) = Self::read_memory_file(&memory_path)
            && !memory.trim().is_empty()
        {
            let truncated = if memory.len() > 2000 {
                let end = memory.floor_char_boundary(2000);
                format!(
                    "{}...\n(use memory tool to read full content)",
                    &memory[..end]
                )
            } else {
                memory
            };
            parts.push(format!("\n## Project Memory\n\n{truncated}"));
        }

        // Inject open tasks summary
        let session_id = self
            .current_session
            .as_ref()
            .map(|s| s.id.as_str())
            .unwrap_or("");
        let task_summary = self.task_store.summary_for_prompt(session_id);
        if !task_summary.is_empty() {
            parts.push(format!("\n## Active Tasks\n\n{task_summary}"));
        }

        if !self.agents_files.is_empty() {
            let mut section = String::from("\n---\n\n## Project Instructions (AGENTS.md)\n");
            for file in &self.agents_files {
                let label = file
                    .path
                    .strip_prefix(&self.project.root)
                    .unwrap_or(&file.path)
                    .display();
                section.push_str(&format!("\n### {label}\n\n{}\n", file.content));
            }
            parts.push(section);
        }

        // Inject MCP resource context (if any servers provide resources)
        // Note: This is a sync context, so we use try_lock + cached resources only
        if let Ok(mgr) = self.mcp_manager.try_lock()
            && mgr.has_servers()
        {
            let resources = mgr.all_resources();
            if !resources.is_empty() {
                let mut section = String::from("\n## MCP Context\n");
                let mut total_len = 0;
                for (server_id, resource) in &resources {
                    let name = &resource.name;
                    let desc = resource.description.as_deref().unwrap_or("");
                    let entry = format!("\n- **{server_id}/{name}**: {desc}\n");
                    total_len += entry.len();
                    if total_len > 2000 {
                        section.push_str("\n(additional resources omitted)\n");
                        break;
                    }
                    section.push_str(&entry);
                }
                parts.push(section);
            }

            // Add MCP tool guidance
            let tool_defs = mgr.all_tool_defs();
            if !tool_defs.is_empty() {
                let mut guidance = String::from(
                    "\n## MCP Tools\n\nExternal tools provided by MCP servers. \
                        These tools use prefixed names (`mcp__{server}__{tool}`). Use them when native tools \
                        don't cover the task.\n",
                );
                for (server_id, tool) in &tool_defs {
                    let desc = tool.description.as_deref().unwrap_or("(no description)");
                    let prefixed = crate::mcp::prefixed_tool_name(server_id, &tool.name);
                    guidance.push_str(&format!("\n- `{prefixed}`: {desc}"));
                }
                guidance.push('\n');
                parts.push(guidance);
            }
        }

        if self.input.mode == AgentMode::Plan {
            parts.push("\n---\n\nYou are currently in PLAN mode. You can read files and analyze the codebase, but you CANNOT write, edit, patch, or create files. Focus on planning, analysis, and providing recommendations. If the user asks you to make changes, explain what you would do but note that the user must switch to BUILD mode (via the Tab key) before changes can be applied.".to_string());
        }

        Some(parts.join("\n"))
    }

    /// Read the memory file with a shared lock for safe concurrent access.
    pub(super) fn read_memory_file(path: &std::path::Path) -> Result<String, std::io::Error> {
        use std::io::Read;
        let file = std::fs::File::open(path)?;
        file.lock_shared()?;
        let mut content = String::new();
        (&file).read_to_string(&mut content)?;
        let _ = file.unlock();
        Ok(content)
    }

    /// Determine which model ref to use for compaction/summarization.
    /// Prefers small_model if configured, falls back to current_model.
    pub(super) fn compact_model_ref(&self) -> Option<String> {
        self.config
            .small_model
            .clone()
            .or_else(|| self.current_model.clone())
    }

    /// Validate a model ref from a saved session against the current provider
    /// registry. If it is no longer valid (e.g. the config was updated after
    /// the session was saved), log a warning and fall back to the model
    /// specified in the current config.
    pub(super) fn validated_model_ref(&self, model_ref: &str) -> String {
        let Some(registry) = self.provider_registry.as_ref() else {
            // No registry to validate against — keep the stored model_ref as-is.
            return model_ref.to_string();
        };
        if registry.resolve_model(model_ref).is_ok() {
            return model_ref.to_string();
        }
        let fallback = self
            .config
            .model
            .clone()
            .unwrap_or_else(|| model_ref.to_string());
        tracing::warn!(
            "session model_ref '{}' is no longer valid, falling back to '{}'",
            model_ref,
            fallback
        );
        fallback
    }

    /// Build a transcript of stored messages for the summarizer.
    pub(super) fn build_compact_prompt(&self) -> String {
        let mut transcript = String::new();
        for msg in &self.stored_messages {
            let role_label = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            let text = msg.text_content();
            if !text.is_empty() {
                transcript.push_str(&format!("[{role_label}]: {text}\n\n"));
            }
        }
        transcript
    }

    /// Gather project context for AGENTS.md generation.
    ///
    /// Collects file tree, key config files, current AGENTS.md, and recent
    /// conversation messages to give the LLM enough context to produce a
    /// useful AGENTS.md.
    pub(super) fn gather_project_context(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // 1. File tree (max_depth 4 = root + 3 subdirectory levels, max 200 entries)
        let mut entries: Vec<String> = Vec::new();
        if let Ok(walker) = ignore::WalkBuilder::new(&self.project.root)
            .hidden(true)
            .git_ignore(true)
            .max_depth(Some(4))
            .build()
            .take(200)
            .collect::<Result<Vec<_>, _>>()
        {
            for entry in walker {
                if let Ok(path) = entry.path().strip_prefix(&self.project.root) {
                    entries.push(path.display().to_string());
                }
            }
        }
        if !entries.is_empty() {
            parts.push(format!("## File Tree\n\n```\n{}\n```", entries.join("\n")));
        }

        // 2. Key config files (first 100 lines each)
        let config_files = [
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "go.mod",
            "Makefile",
            "Dockerfile",
            ".gitignore",
        ];
        for name in &config_files {
            let path = self.project.root.join(name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let truncated: String = content.lines().take(100).collect::<Vec<_>>().join("\n");
                parts.push(format!("## {name}\n\n```\n{truncated}\n```"));
            }
        }

        // 3. Current AGENTS.md
        if !self.agents_files.is_empty() {
            for file in &self.agents_files {
                parts.push(format!(
                    "## Current AGENTS.md ({})\n\n{}",
                    file.path.display(),
                    file.content
                ));
            }
        } else {
            parts.push("## Current AGENTS.md\n\n(No AGENTS.md exists yet)".to_string());
        }

        // 4. Recent conversation (last 5 user messages)
        let user_msgs: Vec<&Message> = self
            .stored_messages
            .iter()
            .filter(|m| m.role == Role::User)
            .collect();
        let recent: Vec<&Message> = user_msgs.iter().rev().take(5).rev().copied().collect();
        if !recent.is_empty() {
            let mut convo = String::from("## Recent Conversation\n\n");
            for msg in recent {
                let text = msg.text_content();
                if !text.is_empty() {
                    convo.push_str(&format!("[User]: {text}\n\n"));
                }
            }
            parts.push(convo);
        }

        // Cap total output at ~10K chars
        let mut result = parts.join("\n\n");
        if result.len() > 10_000 {
            let boundary = result.floor_char_boundary(10_000);
            result.truncate(boundary);
            result.push_str("\n\n(truncated)");
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::make_test_app;

    #[test]
    fn system_prompt_includes_tool_guidance() {
        let app = make_test_app();
        let prompt = app.build_system_prompt().unwrap();
        assert!(
            prompt.contains("Search before reading"),
            "should contain search guidance"
        );
        assert!(prompt.contains("offset"), "should mention offset param");
        assert!(
            prompt.contains("context-efficient"),
            "should mention context efficiency"
        );
        assert!(
            prompt.contains("You are Steve"),
            "should contain Steve identity"
        );
        assert!(
            prompt.contains("Build mode"),
            "should explain permission model"
        );
        assert!(prompt.contains("Date"), "should contain current date");
    }
}
