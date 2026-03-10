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

    /// Delete a session entirely — removes the session metadata file, all its
    /// messages, and clears `last_session_id` from project meta if it pointed
    /// to this session.
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.delete_messages(session_id)?;
        self.storage.delete(&["sessions", session_id])?;
        // Best-effort removal of the now-empty messages directory
        let msg_dir = self.storage.base_dir().join("messages").join(session_id);
        if msg_dir.exists() {
            let _ = std::fs::remove_dir(&msg_dir);
        }
        // Clear last_session_id if it pointed to this session
        let meta = self.load_project_meta();
        if meta.last_session_id.as_deref() == Some(session_id) {
            let mut meta = meta;
            meta.last_session_id = None;
            self.save_project_meta(&meta)?;
        }
        Ok(())
    }

    /// Delete all messages for a session.
    pub fn delete_messages(&self, session_id: &str) -> Result<()> {
        let ids = self.storage.list(&["messages", session_id])?;
        for id in &ids {
            self.storage.delete(&["messages", session_id, id])?;
        }
        Ok(())
    }

    /// Reset token usage counters on the session and save.
    pub fn reset_usage(&self, session: &mut SessionInfo) -> Result<()> {
        session.token_usage = TokenUsage::default();
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::message::Message;
    use crate::storage::Storage;

    use super::*;

    /// Run a test with a properly scoped SessionManager. Storage lives on the
    /// stack so it outlives the manager — no `Box::leak` needed.
    fn with_test_manager(f: impl FnOnce(SessionManager<'_>)) {
        let dir = tempdir().expect("failed to create temp dir");
        let storage = Storage::with_base(dir.path().to_path_buf()).expect("storage");
        let mgr = SessionManager::new(&storage, "test-project");
        f(mgr);
    }

    #[test]
    fn create_and_load_session_roundtrip() {
        with_test_manager(|mgr| {
            let session = mgr.create_session("openai/gpt-4o").expect("create");
            let loaded = mgr.load_session(&session.id).expect("load");
            assert_eq!(loaded.title, "New session");
            assert_eq!(loaded.model_ref, "openai/gpt-4o");
            assert_eq!(loaded.id, session.id);
            assert_eq!(loaded.project_id, "test-project");
        });
    }

    #[test]
    fn list_sessions_sorted_by_updated_at() {
        with_test_manager(|mgr| {
            let s1 = mgr.create_session("m/a").expect("create s1");
            std::thread::sleep(Duration::from_millis(10));
            let s2 = mgr.create_session("m/b").expect("create s2");
            std::thread::sleep(Duration::from_millis(10));
            let s3 = mgr.create_session("m/c").expect("create s3");

            let sessions = mgr.list_sessions().expect("list");
            assert_eq!(sessions.len(), 3);
            // Most recent first
            assert_eq!(sessions[0].id, s3.id);
            assert_eq!(sessions[1].id, s2.id);
            assert_eq!(sessions[2].id, s1.id);
        });
    }

    #[test]
    fn save_and_load_message_roundtrip() {
        with_test_manager(|mgr| {
            let session = mgr.create_session("m/x").expect("create");
            let msg = Message::user(&session.id, "hello");
            mgr.save_message(&msg).expect("save msg");

            let messages = mgr.load_messages(&session.id).expect("load msgs");
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].text_content(), "hello");
            assert_eq!(messages[0].id, msg.id);
        });
    }

    #[test]
    fn messages_sorted_chronologically() {
        with_test_manager(|mgr| {
            let session = mgr.create_session("m/x").expect("create");

        let m1 = Message::user(&session.id, "first");
        mgr.save_message(&m1).expect("save m1");
        std::thread::sleep(Duration::from_millis(10));

        let m2 = Message::user(&session.id, "second");
        mgr.save_message(&m2).expect("save m2");
        std::thread::sleep(Duration::from_millis(10));

        let m3 = Message::user(&session.id, "third");
        mgr.save_message(&m3).expect("save m3");

            let messages = mgr.load_messages(&session.id).expect("load");
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0].text_content(), "first");
            assert_eq!(messages[1].text_content(), "second");
            assert_eq!(messages[2].text_content(), "third");
        });
    }

    #[test]
    fn rename_updates_title() {
        with_test_manager(|mgr| {
            let mut session = mgr.create_session("m/x").expect("create");
            assert_eq!(session.title, "New session");

            mgr.rename_session(&mut session, "New Title").expect("rename");
            assert_eq!(session.title, "New Title");

            // Reload from storage to confirm persistence
            let reloaded = mgr.load_session(&session.id).expect("load");
            assert_eq!(reloaded.title, "New Title");
        });
    }

    #[test]
    fn touch_updates_timestamp() {
        with_test_manager(|mgr| {
            let mut session = mgr.create_session("m/x").expect("create");
            let before = session.updated_at;

            std::thread::sleep(Duration::from_millis(10));
            mgr.touch_session(&mut session).expect("touch");

            assert!(session.updated_at > before);

            // Verify persisted
            let reloaded = mgr.load_session(&session.id).expect("load");
            assert!(reloaded.updated_at > before);
        });
    }

    #[test]
    fn add_usage_accumulates() {
        with_test_manager(|mgr| {
            let mut session = mgr.create_session("m/x").expect("create");

            mgr.add_usage(&mut session, 100, 50).expect("add_usage 1");
            mgr.add_usage(&mut session, 200, 100).expect("add_usage 2");

            assert_eq!(session.token_usage.prompt_tokens, 300);
            assert_eq!(session.token_usage.completion_tokens, 150);
            assert_eq!(session.token_usage.total_tokens, 450);

            // Verify persisted
            let reloaded = mgr.load_session(&session.id).expect("load");
            assert_eq!(reloaded.token_usage.prompt_tokens, 300);
            assert_eq!(reloaded.token_usage.completion_tokens, 150);
            assert_eq!(reloaded.token_usage.total_tokens, 450);
        });
    }

    #[test]
    fn delete_messages_clears_all() {
        with_test_manager(|mgr| {
            let session = mgr.create_session("m/x").expect("create");

            for text in &["one", "two", "three"] {
                let msg = Message::user(&session.id, text);
                mgr.save_message(&msg).expect("save");
            }
            assert_eq!(mgr.load_messages(&session.id).expect("load").len(), 3);

            mgr.delete_messages(&session.id).expect("delete");
            let after = mgr.load_messages(&session.id).expect("load after delete");
            assert!(after.is_empty());
        });
    }

    #[test]
    fn delete_session_removes_session_and_messages() {
        with_test_manager(|mgr| {
            let session = mgr.create_session("m/x").expect("create");
            let msg = Message::user(&session.id, "hello");
            mgr.save_message(&msg).expect("save msg");

            // Session and messages exist
            assert!(mgr.load_session(&session.id).is_ok());
            assert_eq!(mgr.load_messages(&session.id).expect("load").len(), 1);

            mgr.delete_session(&session.id).expect("delete session");

            // Session metadata gone
            assert!(mgr.load_session(&session.id).is_err());
            // Messages gone
            assert!(mgr.load_messages(&session.id).expect("load").is_empty());
            // Not listed
            assert!(!mgr.list_session_ids().expect("list").contains(&session.id));
        });
    }

    #[test]
    fn delete_session_with_no_messages() {
        with_test_manager(|mgr| {
            let session = mgr.create_session("m/x").expect("create");
            // No messages saved — this is the typical prune scenario
            mgr.delete_session(&session.id).expect("delete empty session");
            assert!(mgr.load_session(&session.id).is_err());
            assert!(mgr.load_messages(&session.id).expect("load").is_empty());
        });
    }

    #[test]
    fn delete_session_clears_last_session_id() {
        with_test_manager(|mgr| {
            let session = mgr.create_session("m/x").expect("create");

            // create_session sets last_session_id
            let meta = mgr.load_project_meta();
            assert_eq!(meta.last_session_id.as_deref(), Some(session.id.as_str()));

            mgr.delete_session(&session.id).expect("delete");

            // last_session_id should be cleared
            let meta = mgr.load_project_meta();
            assert!(meta.last_session_id.is_none());
        });
    }

    #[test]
    fn delete_session_preserves_other_sessions_last_id() {
        with_test_manager(|mgr| {
            let s1 = mgr.create_session("m/a").expect("create s1");
            let s2 = mgr.create_session("m/b").expect("create s2");

            // last_session_id points to s2
            let meta = mgr.load_project_meta();
            assert_eq!(meta.last_session_id.as_deref(), Some(s2.id.as_str()));

            // Delete s1 — should NOT clear last_session_id (it points to s2)
            mgr.delete_session(&s1.id).expect("delete s1");

            let meta = mgr.load_project_meta();
            assert_eq!(meta.last_session_id.as_deref(), Some(s2.id.as_str()));

            // s2 still loadable
            assert!(mgr.load_session(&s2.id).is_ok());
        });
    }
}
