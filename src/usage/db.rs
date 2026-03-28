use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::types::{
    ApiCallDetail, ApiCallRecord, ProjectInfo, ProjectRecord, SessionFilter, SessionRecord,
    SessionSummary, UsageStats,
};

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

// ── Read-side queries (used by `steve data` TUI) ────────────

/// Build a WHERE clause and params from a SessionFilter.
///
/// Returns (clause_string, params_vec) where clause_string is either
/// empty or starts with " WHERE ...".
fn build_filter_clause(filter: &SessionFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(ref pid) = filter.project_id {
        conditions.push(format!("s.project_id = ?{}", params.len() + 1));
        params.push(Box::new(pid.clone()));
    }
    if let Some(ref model) = filter.model_ref {
        conditions.push(format!("a.model_ref = ?{}", params.len() + 1));
        params.push(Box::new(model.clone()));
    }
    if let Some(ref from) = filter.date_from {
        conditions.push(format!("s.created_at >= ?{}", params.len() + 1));
        params.push(Box::new(from.clone()));
    }
    if let Some(ref to) = filter.date_to {
        conditions.push(format!("s.created_at <= ?{}", params.len() + 1));
        params.push(Box::new(to.clone()));
    }

    let clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    (clause, params)
}

/// Query sessions with aggregated API call stats, applying optional filters.
pub fn query_sessions(conn: &Connection, filter: &SessionFilter) -> Result<Vec<SessionSummary>> {
    let (where_clause, params) = build_filter_clause(filter);

    let sql = format!(
        "SELECT s.session_id, s.project_id, p.display_name, s.title, s.model_ref, s.created_at, \
         COUNT(a.id) as call_count, \
         COALESCE(SUM(a.prompt_tokens), 0) as total_prompt, \
         COALESCE(SUM(a.completion_tokens), 0) as total_completion, \
         COALESCE(SUM(a.total_tokens), 0) as total_tokens, \
         SUM(a.cost) as total_cost, \
         COALESCE(SUM(a.duration_ms), 0) as total_duration \
         FROM sessions s \
         JOIN projects p ON s.project_id = p.project_id \
         LEFT JOIN api_calls a ON s.session_id = a.session_id \
         {where_clause} \
         GROUP BY s.session_id \
         ORDER BY s.created_at DESC"
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(SessionSummary {
            session_id: row.get(0)?,
            project_id: row.get(1)?,
            project_name: row.get(2)?,
            title: row.get(3)?,
            model_ref: row.get(4)?,
            created_at: row.get(5)?,
            call_count: row.get::<_, i64>(6)? as u32,
            total_prompt_tokens: row.get::<_, i64>(7)? as u64,
            total_completion_tokens: row.get::<_, i64>(8)? as u64,
            total_tokens: row.get::<_, i64>(9)? as u64,
            total_cost: row.get(10)?,
            total_duration_ms: row.get::<_, i64>(11)? as u64,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Query all API calls for a session, ordered by timestamp.
pub fn query_api_calls(conn: &Connection, session_id: &str) -> Result<Vec<ApiCallDetail>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, model_ref, prompt_tokens, completion_tokens, total_tokens, \
         cost, duration_ms, iteration \
         FROM api_calls WHERE session_id = ?1 ORDER BY timestamp ASC, id ASC",
    )?;

    let rows = stmt.query_map([session_id], |row| {
        Ok(ApiCallDetail {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            model_ref: row.get(2)?,
            prompt_tokens: row.get::<_, i64>(3)? as u32,
            completion_tokens: row.get::<_, i64>(4)? as u32,
            total_tokens: row.get::<_, i64>(5)? as u32,
            cost: row.get(6)?,
            duration_ms: row.get::<_, i64>(7)? as u64,
            iteration: row.get::<_, i64>(8)? as u32,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Aggregate usage stats matching the current filter.
pub fn query_usage_stats(conn: &Connection, filter: &SessionFilter) -> Result<UsageStats> {
    let (where_clause, params) = build_filter_clause(filter);

    let sql = format!(
        "SELECT COUNT(DISTINCT s.session_id), COUNT(a.id), \
         COALESCE(SUM(a.total_tokens), 0), COALESCE(SUM(a.cost), 0.0) \
         FROM sessions s \
         JOIN projects p ON s.project_id = p.project_id \
         LEFT JOIN api_calls a ON s.session_id = a.session_id \
         {where_clause}"
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let (session_count, call_count, total_tokens, total_cost): (i64, i64, i64, f64) = conn
        .query_row(&sql, param_refs.as_slice(), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;

    // Query distinct models matching the same filter (not global)
    let models_sql = format!(
        "SELECT DISTINCT a.model_ref FROM api_calls a \
         JOIN sessions s ON a.session_id = s.session_id \
         JOIN projects p ON s.project_id = p.project_id \
         {where_clause} \
         ORDER BY a.model_ref ASC"
    );
    let (_, model_params) = build_filter_clause(filter);
    let model_param_refs: Vec<&dyn rusqlite::types::ToSql> =
        model_params.iter().map(|p| p.as_ref()).collect();
    let mut model_stmt = conn.prepare(&models_sql)?;
    let model_rows = model_stmt.query_map(model_param_refs.as_slice(), |row| row.get(0))?;
    let mut models = Vec::new();
    for row in model_rows {
        models.push(row?);
    }

    Ok(UsageStats {
        session_count: session_count as u32,
        call_count: call_count as u32,
        total_tokens: total_tokens as u64,
        total_cost,
        models_used: models,
    })
}

/// All projects with session counts for filter UI.
pub fn query_projects(conn: &Connection) -> Result<Vec<ProjectInfo>> {
    let mut stmt = conn.prepare(
        "SELECT p.project_id, p.display_name, COUNT(s.session_id) as session_count \
         FROM projects p \
         LEFT JOIN sessions s ON p.project_id = s.project_id \
         GROUP BY p.project_id \
         ORDER BY p.display_name ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(ProjectInfo {
            project_id: row.get(0)?,
            display_name: row.get(1)?,
            session_count: row.get::<_, i64>(2)? as u32,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Distinct model_ref values for the filter UI.
pub fn query_distinct_models(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT model_ref FROM api_calls ORDER BY model_ref ASC")?;

    let rows = stmt.query_map([], |row| row.get(0))?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Open a database in read-only mode (for the data TUI).
pub fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open usage database: {}", path.display()))?;
    Ok(conn)
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

    // ── Query function tests ────────────────────────────────

    /// Seed a second project + session for multi-project tests.
    fn setup_second_project_and_session(conn: &Connection) {
        upsert_project(
            conn,
            &ProjectRecord {
                project_id: "proj-2".into(),
                display_name: "Second Project".into(),
                root_path: "/tmp/second".into(),
            },
        )
        .unwrap();
        upsert_session(
            conn,
            &SessionRecord {
                session_id: "sess-2".into(),
                project_id: "proj-2".into(),
                title: "Second Session".into(),
                model_ref: "anthropic/claude-3".into(),
                created_at: Utc::now(),
            },
        )
        .unwrap();
    }

    /// Insert N api calls for a session with predictable values.
    fn seed_api_calls(conn: &Connection, session_id: &str, project_id: &str, model: &str, n: u32) {
        for i in 0..n {
            insert_api_call(
                conn,
                &ApiCallRecord {
                    timestamp: Utc::now(),
                    project_id: project_id.into(),
                    session_id: session_id.into(),
                    model_ref: model.into(),
                    prompt_tokens: 1000 + i * 100,
                    completion_tokens: 200 + i * 10,
                    total_tokens: 1200 + i * 110,
                    cost: Some(0.01 * (i + 1) as f64),
                    duration_ms: 500 + i as u64 * 100,
                    iteration: i,
                },
            )
            .unwrap();
        }
    }

    #[test]
    fn query_sessions_returns_all_with_empty_filter() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        seed_api_calls(&conn, "sess-1", "proj-1", "openai/gpt-4o", 3);

        let filter = SessionFilter::default();
        let sessions = query_sessions(&conn, &filter).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sess-1");
        assert_eq!(sessions[0].call_count, 3);
        assert_eq!(sessions[0].project_name, "Test Project");
        assert!(sessions[0].total_tokens > 0);
        assert!(sessions[0].total_cost.unwrap() > 0.0);
    }

    #[test]
    fn query_sessions_filters_by_project() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        setup_second_project_and_session(&conn);
        seed_api_calls(&conn, "sess-1", "proj-1", "openai/gpt-4o", 2);
        seed_api_calls(&conn, "sess-2", "proj-2", "anthropic/claude-3", 3);

        let filter = SessionFilter {
            project_id: Some("proj-2".into()),
            ..Default::default()
        };
        let sessions = query_sessions(&conn, &filter).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_id, "proj-2");
        assert_eq!(sessions[0].call_count, 3);
    }

    #[test]
    fn query_sessions_filters_by_model() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        setup_second_project_and_session(&conn);
        seed_api_calls(&conn, "sess-1", "proj-1", "openai/gpt-4o", 2);
        seed_api_calls(&conn, "sess-2", "proj-2", "anthropic/claude-3", 1);

        let filter = SessionFilter {
            model_ref: Some("anthropic/claude-3".into()),
            ..Default::default()
        };
        let sessions = query_sessions(&conn, &filter).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sess-2");
    }

    #[test]
    fn query_sessions_with_no_api_calls() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        // No api_calls seeded

        let sessions = query_sessions(&conn, &SessionFilter::default()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].call_count, 0);
        assert_eq!(sessions[0].total_tokens, 0);
        assert!(sessions[0].total_cost.is_none());
    }

    #[test]
    fn query_api_calls_returns_ordered() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        seed_api_calls(&conn, "sess-1", "proj-1", "openai/gpt-4o", 5);

        let calls = query_api_calls(&conn, "sess-1").unwrap();
        assert_eq!(calls.len(), 5);
        // Iterations should be in order
        for (i, call) in calls.iter().enumerate() {
            assert_eq!(call.iteration, i as u32);
        }
    }

    #[test]
    fn query_api_calls_empty_for_unknown_session() {
        let conn = open_in_memory().unwrap();
        let calls = query_api_calls(&conn, "nonexistent").unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn query_usage_stats_aggregates_correctly() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        setup_second_project_and_session(&conn);
        seed_api_calls(&conn, "sess-1", "proj-1", "openai/gpt-4o", 3);
        seed_api_calls(&conn, "sess-2", "proj-2", "anthropic/claude-3", 2);

        let stats = query_usage_stats(&conn, &SessionFilter::default()).unwrap();
        assert_eq!(stats.session_count, 2);
        assert_eq!(stats.call_count, 5);
        assert!(stats.total_tokens > 0);
        assert!(stats.total_cost > 0.0);
        assert_eq!(stats.models_used.len(), 2);
    }

    #[test]
    fn query_projects_returns_all() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        setup_second_project_and_session(&conn);

        let projects = query_projects(&conn).unwrap();
        assert_eq!(projects.len(), 2);
        // Ordered by display_name ASC
        assert_eq!(projects[0].display_name, "Second Project");
        assert_eq!(projects[1].display_name, "Test Project");
        assert_eq!(projects[0].session_count, 1);
        assert_eq!(projects[1].session_count, 1);
    }

    #[test]
    fn query_distinct_models_returns_unique() {
        let conn = open_in_memory().unwrap();
        setup_test_session(&conn);
        setup_second_project_and_session(&conn);
        seed_api_calls(&conn, "sess-1", "proj-1", "openai/gpt-4o", 3);
        seed_api_calls(&conn, "sess-2", "proj-2", "anthropic/claude-3", 2);

        let models = query_distinct_models(&conn).unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.contains(&"openai/gpt-4o".to_string()));
        assert!(models.contains(&"anthropic/claude-3".to_string()));
    }

    #[test]
    fn query_distinct_models_empty_when_no_calls() {
        let conn = open_in_memory().unwrap();
        let models = query_distinct_models(&conn).unwrap();
        assert!(models.is_empty());
    }
}
