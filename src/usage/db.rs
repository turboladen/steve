use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::types::{ApiCallRecord, ProjectRecord, SessionRecord};

/// Current schema version.
const SCHEMA_VERSION: i64 = 1;

/// Open (or create) the usage database and run any pending migrations.
pub fn open_and_migrate(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open usage database: {}", path.display()))?;

    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let version = current_version(&conn)?;
    if version < SCHEMA_VERSION {
        migrate_to_v1(&conn)?;
    }

    Ok(conn)
}

/// Open an in-memory database with the full schema (for tests).
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    migrate_to_v1(&conn)?;
    Ok(conn)
}

/// Read the current schema version (0 if table doesn't exist yet).
fn current_version(conn: &Connection) -> Result<i64> {
    // Check if schema_version table exists
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_version'",
        [],
        |row| row.get(0),
    )?;

    if !exists {
        return Ok(0);
    }

    let version: i64 = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap_or(0);

    Ok(version)
}

/// Create the full v1 schema in a transaction.
fn migrate_to_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "BEGIN;

        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        );

        DELETE FROM schema_version;
        INSERT INTO schema_version (version) VALUES (1);

        CREATE TABLE IF NOT EXISTS projects (
            project_id   TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            root_path    TEXT NOT NULL,
            first_seen   TEXT NOT NULL,
            last_seen    TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(project_id),
            title      TEXT NOT NULL,
            model_ref  TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS api_calls (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp         TEXT NOT NULL,
            project_id        TEXT NOT NULL REFERENCES projects(project_id),
            session_id        TEXT NOT NULL REFERENCES sessions(session_id),
            model_ref         TEXT NOT NULL,
            prompt_tokens     INTEGER NOT NULL,
            completion_tokens INTEGER NOT NULL,
            total_tokens      INTEGER NOT NULL,
            cost              REAL,
            duration_ms       INTEGER NOT NULL,
            iteration         INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_api_calls_project   ON api_calls(project_id);
        CREATE INDEX IF NOT EXISTS idx_api_calls_session   ON api_calls(session_id);
        CREATE INDEX IF NOT EXISTS idx_api_calls_timestamp ON api_calls(timestamp);
        CREATE INDEX IF NOT EXISTS idx_api_calls_model     ON api_calls(model_ref);

        COMMIT;",
    )
    .context("failed to run v1 migration")?;

    Ok(())
}

/// Insert a single API call record.
pub fn insert_api_call(conn: &Connection, record: &ApiCallRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO api_calls (timestamp, project_id, session_id, model_ref, \
         prompt_tokens, completion_tokens, total_tokens, cost, duration_ms, iteration) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            record.timestamp.to_rfc3339(),
            record.project_id,
            record.session_id,
            record.model_ref,
            record.prompt_tokens,
            record.completion_tokens,
            record.total_tokens,
            record.cost,
            record.duration_ms as i64,
            record.iteration,
        ],
    )?;
    Ok(())
}

/// Insert or update a project record (updates last_seen on conflict).
pub fn upsert_project(conn: &Connection, record: &ProjectRecord) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO projects (project_id, display_name, root_path, first_seen, last_seen) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(project_id) DO UPDATE SET \
         display_name = excluded.display_name, \
         root_path = excluded.root_path, \
         last_seen = excluded.last_seen",
        rusqlite::params![
            record.project_id,
            record.display_name,
            record.root_path,
            now,
            now,
        ],
    )?;
    Ok(())
}

/// Insert or update a session record.
pub fn upsert_session(conn: &Connection, record: &SessionRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (session_id, project_id, title, model_ref, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(session_id) DO UPDATE SET \
         title = excluded.title, \
         model_ref = excluded.model_ref",
        rusqlite::params![
            record.session_id,
            record.project_id,
            record.title,
            record.model_ref,
            record.created_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Update just the title of an existing session.
pub fn update_session_title(conn: &Connection, session_id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET title = ?1 WHERE session_id = ?2",
        rusqlite::params![title, session_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn setup_test_project(conn: &Connection) {
        upsert_project(
            conn,
            &ProjectRecord {
                project_id: "proj-1".into(),
                display_name: "Test Project".into(),
                root_path: "/tmp/test".into(),
            },
        )
        .unwrap();
    }

    fn setup_test_session(conn: &Connection) {
        setup_test_project(conn);
        upsert_session(
            conn,
            &SessionRecord {
                session_id: "sess-1".into(),
                project_id: "proj-1".into(),
                title: "Test Session".into(),
                model_ref: "openai/gpt-4o".into(),
                created_at: Utc::now(),
            },
        )
        .unwrap();
    }

    #[test]
    fn open_in_memory_creates_schema() {
        let conn = open_in_memory().unwrap();
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn schema_has_all_tables() {
        let conn = open_in_memory().unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"projects".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"api_calls".to_string()));
        assert!(tables.contains(&"schema_version".to_string()));
    }

    #[test]
    fn insert_and_query_api_call() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);

        let record = ApiCallRecord {
            timestamp: Utc::now(),
            project_id: "proj-1".into(),
            session_id: "sess-1".into(),
            model_ref: "openai/gpt-4o".into(),
            prompt_tokens: 1000,
            completion_tokens: 200,
            total_tokens: 1200,
            cost: Some(0.0065),
            duration_ms: 1500,
            iteration: 0,
        };
        insert_api_call(&conn, &record).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let (prompt, completion, total): (i64, i64, i64) = conn
            .query_row(
                "SELECT prompt_tokens, completion_tokens, total_tokens FROM api_calls WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(prompt, 1000);
        assert_eq!(completion, 200);
        assert_eq!(total, 1200);
    }

    #[test]
    fn upsert_project_inserts_and_updates() {
        let conn = open_in_memory().unwrap();

        let record = ProjectRecord {
            project_id: "proj-1".into(),
            display_name: "Original Name".into(),
            root_path: "/tmp/original".into(),
        };
        upsert_project(&conn, &record).unwrap();

        let name: String = conn
            .query_row(
                "SELECT display_name FROM projects WHERE project_id = 'proj-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "Original Name");

        // Upsert again with new name
        let updated = ProjectRecord {
            project_id: "proj-1".into(),
            display_name: "Updated Name".into(),
            root_path: "/tmp/updated".into(),
        };
        upsert_project(&conn, &updated).unwrap();

        let name: String = conn
            .query_row(
                "SELECT display_name FROM projects WHERE project_id = 'proj-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "Updated Name");

        // Should still be exactly one row
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn upsert_session_inserts_and_updates_title() {
        let conn = open_in_memory().unwrap();
        setup_test_project(&conn);

        let record = SessionRecord {
            session_id: "sess-1".into(),
            project_id: "proj-1".into(),
            title: "Original Title".into(),
            model_ref: "openai/gpt-4o".into(),
            created_at: Utc::now(),
        };
        upsert_session(&conn, &record).unwrap();

        // Upsert with updated title
        let updated = SessionRecord {
            session_id: "sess-1".into(),
            project_id: "proj-1".into(),
            title: "Updated Title".into(),
            model_ref: "openai/gpt-4o".into(),
            created_at: Utc::now(),
        };
        upsert_session(&conn, &updated).unwrap();

        let title: String = conn
            .query_row(
                "SELECT title FROM sessions WHERE session_id = 'sess-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "Updated Title");
    }

    #[test]
    fn update_session_title_works() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);

        update_session_title(&conn, "sess-1", "New Title").unwrap();

        let title: String = conn
            .query_row(
                "SELECT title FROM sessions WHERE session_id = 'sess-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "New Title");
    }

    #[test]
    fn api_call_with_null_cost() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);

        let record = ApiCallRecord {
            timestamp: Utc::now(),
            project_id: "proj-1".into(),
            session_id: "sess-1".into(),
            model_ref: "openai/gpt-4o".into(),
            prompt_tokens: 500,
            completion_tokens: 100,
            total_tokens: 600,
            cost: None,
            duration_ms: 800,
            iteration: 0,
        };
        insert_api_call(&conn, &record).unwrap();

        let cost: Option<f64> = conn
            .query_row("SELECT cost FROM api_calls WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(cost.is_none());
    }

    #[test]
    fn multiple_api_calls_per_session() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);

        for i in 0..5 {
            let record = ApiCallRecord {
                timestamp: Utc::now(),
                project_id: "proj-1".into(),
                session_id: "sess-1".into(),
                model_ref: "openai/gpt-4o".into(),
                prompt_tokens: 1000 + i * 100,
                completion_tokens: 200,
                total_tokens: 1200 + i * 100,
                cost: Some(0.005),
                duration_ms: 1000 + i as u64 * 200,
                iteration: i,
            };
            insert_api_call(&conn, &record).unwrap();
        }

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM api_calls WHERE session_id = 'sess-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn foreign_key_enforcement() {
        let conn = open_in_memory().unwrap();

        // Inserting a session without a valid project should fail
        let result = upsert_session(
            &conn,
            &SessionRecord {
                session_id: "orphan-sess".into(),
                project_id: "nonexistent".into(),
                title: "Orphan".into(),
                model_ref: "test/model".into(),
                created_at: Utc::now(),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn idempotent_migration() {
        let conn = open_in_memory().unwrap();
        // Running migration again should not fail
        migrate_to_v1(&conn).unwrap();
        // Running migration again should not fail
        migrate_to_v1(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
}
