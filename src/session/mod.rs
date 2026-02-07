pub mod message;
pub mod types;

use anyhow::Result;
use chrono::Utc;

use crate::storage::Storage;
use message::Message;
use types::{SessionInfo, TokenUsage};

/// Project-level metadata persisted in `project.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ProjectMeta {
    /// Last selected model ref.
    pub last_model: Option<String>,
    /// Last active session ID.
    pub last_session_id: Option<String>,
}

/// Session manager. Handles CRUD and message persistence via Storage.
pub struct SessionManager<'a> {
    storage: &'a Storage,
    project_id: String,
}

impl<'a> SessionManager<'a> {
    pub fn new(storage: &'a Storage, project_id: &str) -> Self {
        Self {
            storage,
            project_id: project_id.to_string(),
        }
    }

    // -- Project metadata --

    /// Load the project-level metadata (last model, last session).
    pub fn load_project_meta(&self) -> ProjectMeta {
        self.storage
            .read::<ProjectMeta>(&["project"])
            .unwrap_or_default()
    }

    /// Save the project-level metadata.
    pub fn save_project_meta(&self, meta: &ProjectMeta) -> Result<()> {
        self.storage.write(&["project"], meta)
    }

    // -- Sessions --

    /// Create a new session with the given model ref.
    pub fn create_session(&self, model_ref: &str) -> Result<SessionInfo> {
        let now = Utc::now();
        let session = SessionInfo {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: self.project_id.clone(),
            title: "New session".to_string(),
            model_ref: model_ref.to_string(),
            created_at: now,
            updated_at: now,
            token_usage: TokenUsage::default(),
        };
        self.save_session(&session)?;

        // Update project meta to point at this session
        let mut meta = self.load_project_meta();
        meta.last_session_id = Some(session.id.clone());
        meta.last_model = Some(model_ref.to_string());
        self.save_project_meta(&meta)?;

        Ok(session)
    }

    /// Save/update a session.
    pub fn save_session(&self, session: &SessionInfo) -> Result<()> {
        self.storage.write(&["sessions", &session.id], session)
    }

    /// Load a session by ID.
    pub fn load_session(&self, session_id: &str) -> Result<SessionInfo> {
        self.storage.read(&["sessions", session_id])
    }

    /// List all session IDs.
    pub fn list_session_ids(&self) -> Result<Vec<String>> {
        self.storage.list(&["sessions"])
    }

    /// Load all sessions, sorted by updated_at descending (most recent first).
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let ids = self.list_session_ids()?;
        let mut sessions: Vec<SessionInfo> = ids
            .iter()
            .filter_map(|id| self.load_session(id).ok())
            .collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    // -- Messages --

    /// Save a message for a session.
    pub fn save_message(&self, msg: &Message) -> Result<()> {
        self.storage
            .write(&["messages", &msg.session_id, &msg.id], msg)
    }

    /// Load all messages for a session, sorted by created_at ascending.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let ids = self.storage.list(&["messages", session_id])?;
        let mut messages: Vec<Message> = ids
            .iter()
            .filter_map(|id| {
                self.storage
                    .read::<Message>(&["messages", session_id, id])
                    .ok()
            })
            .collect();
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(messages)
    }

    /// Get the last session for this project (if any).
    pub fn last_session(&self) -> Option<SessionInfo> {
        let meta = self.load_project_meta();
        meta.last_session_id
            .and_then(|id| self.load_session(&id).ok())
    }

    /// Update session's updated_at timestamp and save.
    pub fn touch_session(&self, session: &mut SessionInfo) -> Result<()> {
        session.updated_at = Utc::now();
        self.save_session(session)
    }

    /// Update session title and save.
    pub fn rename_session(&self, session: &mut SessionInfo, title: &str) -> Result<()> {
        session.title = title.to_string();
        session.updated_at = Utc::now();
        self.save_session(session)
    }

    /// Add token usage to the session and save.
    pub fn add_usage(
        &self,
        session: &mut SessionInfo,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) -> Result<()> {
        session.token_usage.prompt_tokens += prompt_tokens as u64;
        session.token_usage.completion_tokens += completion_tokens as u64;
        session.token_usage.total_tokens +=
            (prompt_tokens + completion_tokens) as u64;
        session.updated_at = Utc::now();
        self.save_session(session)
    }
}
