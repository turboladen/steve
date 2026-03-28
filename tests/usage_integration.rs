//! Integration tests for usage analytics & cost tracking.
//!
//! Crosses module boundaries to verify:
//! - File-based write → read pipeline (UsageWriter → SQLite file → query)
//! - Cost calculation consistency (ResolvedModel::session_cost ↔ stored costs)
//! - Data TUI rendering (DataState → views::render → buffer assertions)
//! - Sidebar cost display

use chrono::Utc;
use ratatui::{Frame, Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};
use tempfile::TempDir;

use steve::{
    config::types::{ModelCapabilities, ModelConfig, ModelCost, ProviderConfig},
    data::{
        state::{DataState, View},
        views,
    },
    provider::ResolvedModel,
    ui::{
        sidebar::{SidebarState, render_sidebar},
        theme::Theme,
    },
    usage::{db, spawn_usage_writer, types::*},
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Render a draw closure into a headless test buffer.
fn render_to_buffer(w: u16, h: u16, draw: impl FnOnce(&mut Frame)) -> Buffer {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f)).unwrap();
    terminal.backend().buffer().clone()
}

/// Extract plain text from a buffer region (row by row, trimmed trailing spaces).
fn buffer_text(buf: &Buffer, area: Rect) -> String {
    let mut out = String::new();
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        for x in area.x..area.x + area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// Full-buffer text extraction (convenience).
fn full_buffer_text(buf: &Buffer) -> String {
    let area = Rect::new(0, 0, buf.area().width, buf.area().height);
    buffer_text(buf, area)
}

/// Seed a project via direct DB call.
fn make_project(conn: &rusqlite::Connection, id: &str, name: &str) {
    db::upsert_project(
        conn,
        &ProjectRecord {
            project_id: id.into(),
            display_name: name.into(),
            root_path: format!("/tmp/{id}"),
        },
    )
    .unwrap();
}

/// Seed a session via direct DB call.
fn make_session(
    conn: &rusqlite::Connection,
    sess_id: &str,
    proj_id: &str,
    title: &str,
    model: &str,
) {
    db::upsert_session(
        conn,
        &SessionRecord {
            session_id: sess_id.into(),
            project_id: proj_id.into(),
            title: title.into(),
            model_ref: model.into(),
            created_at: Utc::now(),
        },
    )
    .unwrap();
}

/// Insert `n` API calls with known cost and token values.
fn seed_calls(
    conn: &rusqlite::Connection,
    sess_id: &str,
    proj_id: &str,
    model: &str,
    n: u32,
    cost_per_call: Option<f64>,
) {
    for i in 0..n {
        db::insert_api_call(
            conn,
            &ApiCallRecord {
                timestamp: Utc::now(),
                project_id: proj_id.into(),
                session_id: sess_id.into(),
                model_ref: model.into(),
                prompt_tokens: 1000,
                completion_tokens: 200,
                total_tokens: 1200,
                cost: cost_per_call,
                duration_ms: 500,
                iteration: i,
            },
        )
        .unwrap();
    }
}

/// Build a ResolvedModel with optional cost pricing.
fn make_resolved_model(cost: Option<ModelCost>) -> ResolvedModel {
    ResolvedModel {
        provider_id: "openai".into(),
        model_id: "gpt-4o".into(),
        config: ModelConfig {
            id: "gpt-4o".into(),
            name: "GPT-4o".into(),
            context_window: 128000,
            max_output_tokens: None,
            cost,
            capabilities: ModelCapabilities::default(),
        },
        provider_config: ProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            models: Default::default(),
        },
    }
}

/// Build a SessionSummary for rendering tests.
fn make_session_summary(
    id: &str,
    title: &str,
    model: &str,
    tokens: u64,
    cost: Option<f64>,
) -> SessionSummary {
    SessionSummary {
        session_id: id.into(),
        project_id: "proj-1".into(),
        project_name: "TestProject".into(),
        title: title.into(),
        model_ref: model.into(),
        created_at: "2026-03-10T14:00:00Z".into(),
        call_count: 3,
        total_prompt_tokens: tokens * 8 / 10,
        total_completion_tokens: tokens * 2 / 10,
        total_tokens: tokens,
        total_cost: cost,
        total_duration_ms: 2500,
    }
}

// ===========================================================================
// Group 1: File-Based DB Pipeline
// ===========================================================================

#[test]
fn file_based_write_and_read_pipeline() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("usage.db");

    // Write via UsageWriter (background thread)
    let handle = spawn_usage_writer(&db_path).unwrap();
    let writer = &handle.writer;

    writer.upsert_project(ProjectRecord {
        project_id: "proj-1".into(),
        display_name: "My Project".into(),
        root_path: "/tmp/proj-1".into(),
    });

    writer.upsert_session(SessionRecord {
        session_id: "sess-1".into(),
        project_id: "proj-1".into(),
        title: "First Session".into(),
        model_ref: "openai/gpt-4o".into(),
        created_at: Utc::now(),
    });

    for i in 0..3 {
        writer.record_api_call(ApiCallRecord {
            timestamp: Utc::now(),
            project_id: "proj-1".into(),
            session_id: "sess-1".into(),
            model_ref: "openai/gpt-4o".into(),
            prompt_tokens: 1000,
            completion_tokens: 200,
            total_tokens: 1200,
            cost: Some(0.006),
            duration_ms: 500,
            iteration: i,
        });
    }

    // Flush all writes
    handle.shutdown_and_wait();

    // Read back via open_readonly
    let conn = db::open_readonly(&db_path).unwrap();

    let sessions = db::query_sessions(&conn, &SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "First Session");
    assert_eq!(sessions[0].call_count, 3);
    assert_eq!(sessions[0].total_tokens, 3600); // 1200 * 3

    let calls = db::query_api_calls(&conn, "sess-1").unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].prompt_tokens, 1000);

    let stats = db::query_usage_stats(&conn, &SessionFilter::default()).unwrap();
    assert_eq!(stats.session_count, 1);
    assert_eq!(stats.call_count, 3);
    assert!((stats.total_cost - 0.018).abs() < 1e-10);
}

#[test]
fn file_based_multi_project_aggregation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("usage.db");

    let handle = spawn_usage_writer(&db_path).unwrap();
    let writer = &handle.writer;

    // Project A
    writer.upsert_project(ProjectRecord {
        project_id: "proj-a".into(),
        display_name: "Alpha".into(),
        root_path: "/tmp/alpha".into(),
    });
    writer.upsert_session(SessionRecord {
        session_id: "sess-a".into(),
        project_id: "proj-a".into(),
        title: "Alpha Session".into(),
        model_ref: "openai/gpt-4o".into(),
        created_at: Utc::now(),
    });
    writer.record_api_call(ApiCallRecord {
        timestamp: Utc::now(),
        project_id: "proj-a".into(),
        session_id: "sess-a".into(),
        model_ref: "openai/gpt-4o".into(),
        prompt_tokens: 2000,
        completion_tokens: 400,
        total_tokens: 2400,
        cost: Some(0.012),
        duration_ms: 800,
        iteration: 0,
    });

    // Project B
    writer.upsert_project(ProjectRecord {
        project_id: "proj-b".into(),
        display_name: "Beta".into(),
        root_path: "/tmp/beta".into(),
    });
    writer.upsert_session(SessionRecord {
        session_id: "sess-b".into(),
        project_id: "proj-b".into(),
        title: "Beta Session".into(),
        model_ref: "anthropic/claude".into(),
        created_at: Utc::now(),
    });
    writer.record_api_call(ApiCallRecord {
        timestamp: Utc::now(),
        project_id: "proj-b".into(),
        session_id: "sess-b".into(),
        model_ref: "anthropic/claude".into(),
        prompt_tokens: 5000,
        completion_tokens: 1000,
        total_tokens: 6000,
        cost: Some(0.090),
        duration_ms: 1500,
        iteration: 0,
    });

    handle.shutdown_and_wait();

    let conn = db::open_readonly(&db_path).unwrap();

    // Unfiltered: both projects
    let all = db::query_sessions(&conn, &SessionFilter::default()).unwrap();
    assert_eq!(all.len(), 2);

    // Filter to project A only
    let filter_a = SessionFilter {
        project_id: Some("proj-a".into()),
        ..Default::default()
    };
    let proj_a = db::query_sessions(&conn, &filter_a).unwrap();
    assert_eq!(proj_a.len(), 1);
    assert_eq!(proj_a[0].title, "Alpha Session");

    // Filter to project B only
    let filter_b = SessionFilter {
        project_id: Some("proj-b".into()),
        ..Default::default()
    };
    let proj_b = db::query_sessions(&conn, &filter_b).unwrap();
    assert_eq!(proj_b.len(), 1);
    assert_eq!(proj_b[0].title, "Beta Session");

    // Stats filtered to project B
    let stats_b = db::query_usage_stats(&conn, &filter_b).unwrap();
    assert_eq!(stats_b.session_count, 1);
    assert_eq!(stats_b.total_tokens, 6000);
    assert!((stats_b.total_cost - 0.090).abs() < 1e-10);
}

// ===========================================================================
// Group 2: Cost Calculation
// ===========================================================================

#[test]
fn cost_with_pricing_configured() {
    let model = make_resolved_model(Some(ModelCost {
        input_per_million: 3.0,
        output_per_million: 15.0,
    }));

    // ResolvedModel::session_cost formula
    let cost = model.session_cost(1000, 200);
    let expected = (1000.0 / 1_000_000.0) * 3.0 + (200.0 / 1_000_000.0) * 15.0;
    assert_eq!(cost, Some(expected));
    assert!((expected - 0.006).abs() < 1e-10);

    // Store the same cost in the DB and verify round-trip
    let conn = db::open_in_memory().unwrap();
    make_project(&conn, "proj-1", "Test");
    make_session(&conn, "sess-1", "proj-1", "Cost Test", "openai/gpt-4o");
    db::insert_api_call(
        &conn,
        &ApiCallRecord {
            timestamp: Utc::now(),
            project_id: "proj-1".into(),
            session_id: "sess-1".into(),
            model_ref: "openai/gpt-4o".into(),
            prompt_tokens: 1000,
            completion_tokens: 200,
            total_tokens: 1200,
            cost: Some(expected),
            duration_ms: 500,
            iteration: 0,
        },
    )
    .unwrap();

    let sessions = db::query_sessions(&conn, &SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    let stored_cost = sessions[0].total_cost.unwrap();
    assert!((stored_cost - expected).abs() < 1e-10);
}

#[test]
fn cost_without_pricing_returns_none() {
    let model = make_resolved_model(None);
    assert_eq!(model.session_cost(1000, 200), None);

    // Store API call with no cost, verify query returns None
    let conn = db::open_in_memory().unwrap();
    make_project(&conn, "proj-1", "Test");
    make_session(&conn, "sess-1", "proj-1", "No Cost", "openai/gpt-4o");
    seed_calls(&conn, "sess-1", "proj-1", "openai/gpt-4o", 2, None);

    let sessions = db::query_sessions(&conn, &SessionFilter::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].total_cost, None);
}

#[test]
fn cost_accumulates_across_calls() {
    let conn = db::open_in_memory().unwrap();
    make_project(&conn, "proj-1", "Test");
    make_session(&conn, "sess-1", "proj-1", "Accumulation", "openai/gpt-4o");

    let cost_per_call = 0.006;
    seed_calls(
        &conn,
        "sess-1",
        "proj-1",
        "openai/gpt-4o",
        5,
        Some(cost_per_call),
    );

    let stats = db::query_usage_stats(&conn, &SessionFilter::default()).unwrap();
    assert_eq!(stats.call_count, 5);
    let expected_total = cost_per_call * 5.0;
    assert!(
        (stats.total_cost - expected_total).abs() < 1e-10,
        "expected {expected_total}, got {}",
        stats.total_cost
    );

    // Also verify via session summary
    let sessions = db::query_sessions(&conn, &SessionFilter::default()).unwrap();
    let session_cost = sessions[0].total_cost.unwrap();
    assert!(
        (session_cost - expected_total).abs() < 1e-10,
        "session cost: expected {expected_total}, got {session_cost}"
    );
}

// ===========================================================================
// Group 3: Data TUI Rendering
// ===========================================================================

#[test]
fn render_session_list_shows_titles_and_costs() {
    let theme = Theme::default();
    let mut state = DataState::new();
    state.sessions = vec![
        make_session_summary(
            "s1",
            "Refactor auth module",
            "openai/gpt-4o",
            12000,
            Some(0.060),
        ),
        make_session_summary("s2", "Fix login bug", "anthropic/claude", 6000, Some(1.25)),
    ];
    state.stats = UsageStats {
        session_count: 2,
        call_count: 6,
        total_tokens: 18000,
        total_cost: 1.31,
        models_used: vec!["openai/gpt-4o".into(), "anthropic/claude".into()],
    };

    let buf = render_to_buffer(100, 24, |frame| {
        views::render(frame, &state, &theme);
    });
    let text = full_buffer_text(&buf);

    // Session titles should appear
    assert!(
        text.contains("Refactor auth module"),
        "missing first title in:\n{text}"
    );
    assert!(
        text.contains("Fix login bug"),
        "missing second title in:\n{text}"
    );

    // Cost values should appear (format_cost: <$1 → "$0.060", >=$1 → "$1.25")
    assert!(text.contains("$0.060"), "missing $0.060 in:\n{text}");
    assert!(text.contains("$1.25"), "missing $1.25 in:\n{text}");

    // Stats footer should show aggregated data
    assert!(
        text.contains("2 sessions"),
        "missing session count in:\n{text}"
    );
}

#[test]
fn render_detail_view_shows_call_data() {
    let theme = Theme::default();
    let mut state = DataState::new();
    state.view = View::SessionDetail;
    state.detail_session_title = "Debug rendering".into();
    state.detail_session_model = "openai/gpt-4o".into();
    state.detail_session_date = "2026-03-10T14:00:00Z".into();
    state.detail_calls = vec![
        ApiCallDetail {
            id: 1,
            timestamp: "2026-03-10T14:00:01Z".into(),
            model_ref: "openai/gpt-4o".into(),
            prompt_tokens: 2000,
            completion_tokens: 500,
            total_tokens: 2500,
            cost: Some(0.012),
            duration_ms: 800,
            iteration: 0,
        },
        ApiCallDetail {
            id: 2,
            timestamp: "2026-03-10T14:00:05Z".into(),
            model_ref: "openai/gpt-4o".into(),
            prompt_tokens: 3000,
            completion_tokens: 600,
            total_tokens: 3600,
            cost: Some(0.018),
            duration_ms: 1200,
            iteration: 1,
        },
        ApiCallDetail {
            id: 3,
            timestamp: "2026-03-10T14:00:10Z".into(),
            model_ref: "openai/gpt-4o".into(),
            prompt_tokens: 4000,
            completion_tokens: 800,
            total_tokens: 4800,
            cost: Some(0.024),
            duration_ms: 1500,
            iteration: 2,
        },
    ];

    let buf = render_to_buffer(100, 24, |frame| {
        views::render(frame, &state, &theme);
    });
    let text = full_buffer_text(&buf);

    // Session title in header
    assert!(
        text.contains("Debug rendering"),
        "missing session title in:\n{text}"
    );

    // Iteration numbers (0, 1, 2) should appear in the table
    // Row numbers (1, 2, 3) should appear
    assert!(text.contains("3 calls"), "missing call count in:\n{text}");

    // Token values: 2.5k, 3.6k, 4.8k
    assert!(text.contains("2.5k"), "missing 2.5k tokens in:\n{text}");
    assert!(text.contains("3.6k"), "missing 3.6k tokens in:\n{text}");
    assert!(text.contains("4.8k"), "missing 4.8k tokens in:\n{text}");
}

#[test]
fn render_session_list_shows_null_cost_as_dashes() {
    let theme = Theme::default();
    let mut state = DataState::new();
    state.sessions = vec![make_session_summary(
        "s1",
        "No pricing configured",
        "local/llama",
        5000,
        None,
    )];
    state.stats = UsageStats {
        session_count: 1,
        call_count: 2,
        total_tokens: 5000,
        total_cost: 0.0,
        models_used: vec!["local/llama".into()],
    };

    let buf = render_to_buffer(100, 24, |frame| {
        views::render(frame, &state, &theme);
    });
    let text = full_buffer_text(&buf);

    // None cost renders as "--"
    assert!(
        text.contains("--"),
        "missing '--' for null cost in:\n{text}"
    );
    assert!(
        text.contains("No pricing configured"),
        "missing title in:\n{text}"
    );
}

#[test]
fn render_with_filter_shows_only_matching() {
    // This test verifies that when DataState is loaded with filtered data,
    // only matching sessions appear in the rendered output.
    let conn = db::open_in_memory().unwrap();
    make_project(&conn, "proj-a", "Alpha");
    make_project(&conn, "proj-b", "Beta");
    make_session(&conn, "sess-a", "proj-a", "Alpha Work", "openai/gpt-4o");
    make_session(&conn, "sess-b", "proj-b", "Beta Work", "anthropic/claude");
    seed_calls(&conn, "sess-a", "proj-a", "openai/gpt-4o", 2, Some(0.01));
    seed_calls(&conn, "sess-b", "proj-b", "anthropic/claude", 3, Some(0.02));

    // Query with filter for proj-a
    let filter = SessionFilter {
        project_id: Some("proj-a".into()),
        ..Default::default()
    };
    let sessions = db::query_sessions(&conn, &filter).unwrap();
    let stats = db::query_usage_stats(&conn, &filter).unwrap();

    let theme = Theme::default();
    let mut state = DataState::new();
    state.sessions = sessions;
    state.stats = stats;

    let buf = render_to_buffer(100, 24, |frame| {
        views::render(frame, &state, &theme);
    });
    let text = full_buffer_text(&buf);

    assert!(
        text.contains("Alpha Work"),
        "missing Alpha session in:\n{text}"
    );
    assert!(
        !text.contains("Beta Work"),
        "Beta session should be filtered out:\n{text}"
    );
    assert!(
        text.contains("1 sessions"),
        "stats should reflect filter in:\n{text}"
    );
}

// ===========================================================================
// Group 4: Sidebar Cost Display
// ===========================================================================

#[test]
fn sidebar_renders_cost_and_na() {
    let theme = Theme::default();

    // With cost
    let state_with_cost = SidebarState {
        session_cost: Some(0.0512),
        model_name: "GPT-4o".into(),
        ..Default::default()
    };
    let buf = render_to_buffer(40, 20, |frame| {
        let area = frame.area();
        render_sidebar(frame, area, &state_with_cost, &theme, 0);
    });
    let text = full_buffer_text(&buf);
    assert!(
        text.contains("$0.0512"),
        "missing cost '$0.0512' in:\n{text}"
    );

    // Without cost (no pricing)
    let state_no_cost = SidebarState {
        session_cost: None,
        model_name: "Llama".into(),
        ..Default::default()
    };
    let buf = render_to_buffer(40, 20, |frame| {
        let area = frame.area();
        render_sidebar(frame, area, &state_no_cost, &theme, 0);
    });
    let text = full_buffer_text(&buf);
    assert!(
        text.contains("N/A"),
        "missing 'N/A' for no cost in:\n{text}"
    );
}
