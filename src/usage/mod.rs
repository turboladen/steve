pub mod db;
pub mod types;

use std::path::Path;
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::Result;

use types::{ApiCallRecord, ProjectRecord, SessionRecord, UsageCommand};

/// A cloneable handle for sending usage data to the background writer thread.
///
/// All methods are fire-and-forget — if the writer thread has shut down,
/// sends silently fail (no panics, no blocking).
#[derive(Clone)]
pub struct UsageWriter {
    tx: mpsc::Sender<UsageCommand>,
}

impl UsageWriter {
    /// Record a single API call's usage data.
    pub fn record_api_call(&self, record: ApiCallRecord) {
        let _ = self.tx.send(UsageCommand::RecordApiCall(record));
    }

    /// Insert or update project metadata.
    pub fn upsert_project(&self, record: ProjectRecord) {
        let _ = self.tx.send(UsageCommand::UpsertProject(record));
    }

    /// Insert or update session metadata.
    pub fn upsert_session(&self, record: SessionRecord) {
        let _ = self.tx.send(UsageCommand::UpsertSession(record));
    }

    /// Update the title of an existing session.
    pub fn update_session_title(&self, session_id: &str, title: &str) {
        let _ = self.tx.send(UsageCommand::UpdateSessionTitle {
            session_id: session_id.to_string(),
            title: title.to_string(),
        });
    }

    /// Request a graceful shutdown of the writer thread (fire-and-forget).
    pub fn shutdown(&self) {
        let _ = self.tx.send(UsageCommand::Shutdown);
    }
}

/// Handle returned by `spawn_usage_writer` — owns the writer thread's JoinHandle.
pub struct UsageWriterHandle {
    pub writer: UsageWriter,
    join_handle: Option<JoinHandle<()>>,
}

impl UsageWriterHandle {
    /// Send shutdown command and block until the writer thread exits.
    /// Ensures all queued writes are flushed before returning.
    pub fn shutdown_and_wait(mut self) {
        self.writer.shutdown();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn a background writer thread that owns the SQLite connection.
///
/// Returns a `UsageWriterHandle` containing a cloneable `UsageWriter` and
/// the thread's JoinHandle for graceful shutdown.
pub fn spawn_usage_writer(db_path: &Path) -> Result<UsageWriterHandle> {
    let conn = db::open_and_migrate(db_path)?;
    let (tx, rx) = mpsc::channel();

    let join_handle = std::thread::Builder::new()
        .name("usage-writer".into())
        .spawn(move || {
            writer_loop(conn, rx);
        })?;

    Ok(UsageWriterHandle {
        writer: UsageWriter { tx },
        join_handle: Some(join_handle),
    })
}

/// Create a UsageWriter backed by an in-memory database (for tests).
/// Smoke tests only — SQL correctness is verified in `db::tests`.
pub fn test_usage_writer() -> UsageWriter {
    let conn = db::open_in_memory().expect("in-memory db");
    let (tx, rx) = mpsc::channel();

    std::thread::Builder::new()
        .name("usage-writer-test".into())
        .spawn(move || {
            writer_loop(conn, rx);
        })
        .expect("spawn test writer");

    UsageWriter { tx }
}

/// The main loop for the writer thread. Processes commands until
/// `Shutdown` or channel disconnect.
fn writer_loop(conn: rusqlite::Connection, rx: mpsc::Receiver<UsageCommand>) {
    for cmd in rx.iter() {
        match cmd {
            UsageCommand::RecordApiCall(record) => {
                if let Err(e) = db::insert_api_call(&conn, &record) {
                    tracing::error!(error = %e, "failed to insert API call record");
                }
            }
            UsageCommand::UpsertProject(record) => {
                if let Err(e) = db::upsert_project(&conn, &record) {
                    tracing::error!(error = %e, "failed to upsert project");
                }
            }
            UsageCommand::UpsertSession(record) => {
                if let Err(e) = db::upsert_session(&conn, &record) {
                    tracing::error!(error = %e, "failed to upsert session");
                }
            }
            UsageCommand::UpdateSessionTitle { session_id, title } => {
                if let Err(e) = db::update_session_title(&conn, &session_id, &title) {
                    tracing::error!(error = %e, "failed to update session title");
                }
            }
            UsageCommand::Shutdown => {
                tracing::info!("usage writer shutting down");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn writer_handles_api_call_record() {
        let writer = test_usage_writer();

        writer.upsert_project(ProjectRecord {
            project_id: "proj-1".into(),
            display_name: "Test".into(),
            root_path: "/tmp/test".into(),
        });

        writer.upsert_session(SessionRecord {
            session_id: "sess-1".into(),
            project_id: "proj-1".into(),
            title: "Test Session".into(),
            model_ref: "test/model".into(),
            created_at: Utc::now(),
        });

        writer.record_api_call(ApiCallRecord {
            timestamp: Utc::now(),
            project_id: "proj-1".into(),
            session_id: "sess-1".into(),
            model_ref: "test/model".into(),
            prompt_tokens: 1000,
            completion_tokens: 200,
            total_tokens: 1200,
            cost: Some(0.005),
            duration_ms: 1500,
            iteration: 0,
        });

        writer.shutdown();
        // Give the thread a moment to process
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    #[test]
    fn writer_survives_channel_drop() {
        let writer = test_usage_writer();
        drop(writer);
        // Thread should exit gracefully when channel is dropped
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    #[test]
    fn writer_is_clone_and_send() {
        let writer = test_usage_writer();
        let writer2 = writer.clone();

        writer2.upsert_project(ProjectRecord {
            project_id: "proj-2".into(),
            display_name: "Clone Test".into(),
            root_path: "/tmp/clone".into(),
        });

        writer.shutdown();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    #[test]
    fn update_session_title_via_writer() {
        let writer = test_usage_writer();

        writer.upsert_project(ProjectRecord {
            project_id: "proj-1".into(),
            display_name: "Test".into(),
            root_path: "/tmp/test".into(),
        });

        writer.upsert_session(SessionRecord {
            session_id: "sess-1".into(),
            project_id: "proj-1".into(),
            title: "Original".into(),
            model_ref: "test/model".into(),
            created_at: Utc::now(),
        });

        writer.update_session_title("sess-1", "Renamed Title");
        writer.shutdown();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    #[test]
    fn shutdown_and_wait_blocks_until_thread_exits() {
        let handle = spawn_usage_writer_in_memory();

        handle.writer.upsert_project(ProjectRecord {
            project_id: "proj-1".into(),
            display_name: "Test".into(),
            root_path: "/tmp/test".into(),
        });

        // This should block until all queued writes complete
        handle.shutdown_and_wait();
        // If we get here, the thread exited cleanly
    }

    fn spawn_usage_writer_in_memory() -> UsageWriterHandle {
        let conn = db::open_in_memory().expect("in-memory db");
        let (tx, rx) = mpsc::channel();

        let join_handle = std::thread::Builder::new()
            .name("usage-writer-test".into())
            .spawn(move || {
                writer_loop(conn, rx);
            })
            .expect("spawn test writer");

        UsageWriterHandle {
            writer: UsageWriter { tx },
            join_handle: Some(join_handle),
        }
    }
}
