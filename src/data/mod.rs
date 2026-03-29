pub mod state;
pub mod views;

use std::{path::Path, time::Duration};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use rusqlite::Connection;

use crate::{
    ui::theme::Theme,
    usage::{db, types::SessionFilter},
};

use state::{DataState, FilterField, View};

/// The data browser TUI application.
struct DataApp {
    conn: Connection,
    theme: Theme,
    state: DataState,
    should_quit: bool,
}

impl DataApp {
    fn new(conn: Connection) -> Self {
        Self {
            conn,
            theme: Theme::dark(),
            state: DataState::new(),
            should_quit: false,
        }
    }

    /// Load initial data from the database.
    fn load_data(&mut self) -> Result<()> {
        self.state.projects = db::query_projects(&self.conn)?;
        self.state.models = db::query_distinct_models(&self.conn)?;
        self.refresh_sessions()?;
        Ok(())
    }

    /// Re-query sessions and stats based on current filter.
    fn refresh_sessions(&mut self) -> Result<()> {
        self.state.sessions = db::query_sessions(&self.conn, &self.state.filter)?;
        self.state.stats = db::query_usage_stats(&self.conn, &self.state.filter)?;
        self.state.sort_sessions();
        self.state.clamp_selection();
        Ok(())
    }

    /// Load API call details for the selected session.
    fn load_detail(&mut self) -> Result<()> {
        if let Some(session) = self.state.sessions.get(self.state.selected_row) {
            self.state.detail_calls = db::query_api_calls(&self.conn, &session.session_id)?;
            self.state.detail_session_title = session.title.clone();
            self.state.detail_session_model = session.model_ref.clone();
            self.state.detail_session_date = session.created_at.clone();
            self.state.detail_selected = 0;
            self.state.detail_offset = 0;
            self.state.view = View::SessionDetail;
        }
        Ok(())
    }

    /// Handle a key event. Returns Ok(true) if the event was consumed.
    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        // Global quit
        match (code, modifiers) {
            (KeyCode::Char('q'), KeyModifiers::NONE) if !self.state.filter_active => {
                self.should_quit = true;
                return Ok(());
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
                return Ok(());
            }
            _ => {}
        }

        if self.state.filter_active {
            self.handle_filter_key(code)?;
            return Ok(());
        }

        match self.state.view {
            View::SessionList => self.handle_session_list_key(code)?,
            View::SessionDetail => self.handle_detail_key(code)?,
        }
        Ok(())
    }

    fn handle_session_list_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.state.selected_row > 0 {
                    self.state.selected_row -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.state.selected_row + 1 < self.state.sessions.len() {
                    self.state.selected_row += 1;
                }
            }
            KeyCode::PageUp => {
                self.state.selected_row = self.state.selected_row.saturating_sub(20);
            }
            KeyCode::PageDown => {
                let max = self.state.sessions.len().saturating_sub(1);
                self.state.selected_row = (self.state.selected_row + 20).min(max);
            }
            KeyCode::Home => {
                self.state.selected_row = 0;
            }
            KeyCode::End => {
                self.state.selected_row = self.state.sessions.len().saturating_sub(1);
            }
            KeyCode::Enter => {
                self.load_detail()?;
            }
            KeyCode::Tab => {
                self.state.sort_column = self.state.sort_column.next();
                self.state.sort_sessions();
                self.state.selected_row = 0;
            }
            KeyCode::BackTab => {
                self.state.sort_column = self.state.sort_column.prev();
                self.state.sort_sessions();
                self.state.selected_row = 0;
            }
            KeyCode::Char('r') => {
                self.state.sort_ascending = !self.state.sort_ascending;
                self.state.sort_sessions();
            }
            KeyCode::Char('f') => {
                self.state.filter_active = true;
                self.state.filter_field = FilterField::Project;
            }
            KeyCode::Esc => {
                self.should_quit = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_detail_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.state.detail_selected > 0 {
                    self.state.detail_selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.state.detail_selected + 1 < self.state.detail_calls.len() {
                    self.state.detail_selected += 1;
                }
            }
            KeyCode::PageUp => {
                self.state.detail_selected = self.state.detail_selected.saturating_sub(20);
            }
            KeyCode::PageDown => {
                let max = self.state.detail_calls.len().saturating_sub(1);
                self.state.detail_selected = (self.state.detail_selected + 20).min(max);
            }
            KeyCode::Esc | KeyCode::Backspace => {
                self.state.view = View::SessionList;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_filter_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.state.filter_field = self.state.filter_field.next();
            }
            KeyCode::Left => {
                self.cycle_filter_option(false);
            }
            KeyCode::Right => {
                self.cycle_filter_option(true);
            }
            KeyCode::Enter => {
                self.apply_filter()?;
                self.state.filter_active = false;
            }
            KeyCode::Esc => {
                self.state.filter_active = false;
            }
            _ => {}
        }
        Ok(())
    }

    fn cycle_filter_option(&mut self, forward: bool) {
        match self.state.filter_field {
            FilterField::Project => {
                let count = self.state.projects.len();
                if count == 0 {
                    return;
                }
                self.state.filter_project_idx = match self.state.filter_project_idx {
                    None => {
                        if forward {
                            Some(0)
                        } else {
                            Some(count - 1)
                        }
                    }
                    Some(idx) => {
                        if forward {
                            if idx + 1 >= count {
                                None // wrap to "All"
                            } else {
                                Some(idx + 1)
                            }
                        } else if idx == 0 {
                            None // wrap to "All"
                        } else {
                            Some(idx - 1)
                        }
                    }
                };
            }
            FilterField::Model => {
                let count = self.state.models.len();
                if count == 0 {
                    return;
                }
                self.state.filter_model_idx = match self.state.filter_model_idx {
                    None => {
                        if forward {
                            Some(0)
                        } else {
                            Some(count - 1)
                        }
                    }
                    Some(idx) => {
                        if forward {
                            if idx + 1 >= count {
                                None
                            } else {
                                Some(idx + 1)
                            }
                        } else if idx == 0 {
                            None
                        } else {
                            Some(idx - 1)
                        }
                    }
                };
            }
        }
    }

    fn apply_filter(&mut self) -> Result<()> {
        self.state.filter = SessionFilter {
            project_id: self
                .state
                .filter_project_idx
                .and_then(|i| self.state.projects.get(i))
                .map(|p| p.project_id.clone()),
            model_ref: self
                .state
                .filter_model_idx
                .and_then(|i| self.state.models.get(i).cloned()),
            ..Default::default()
        };
        self.state.selected_row = 0;
        self.state.list_offset = 0;
        self.refresh_sessions()
    }

    /// Keep scroll offset in sync with selection.
    fn update_scroll(&mut self, visible_height: u16) {
        let h = visible_height.saturating_sub(3) as usize; // borders + header
        if h == 0 {
            return;
        }
        match self.state.view {
            View::SessionList => {
                let sel = self.state.selected_row;
                if sel < self.state.list_offset {
                    self.state.list_offset = sel;
                } else if sel >= self.state.list_offset + h {
                    self.state.list_offset = sel - h + 1;
                }
            }
            View::SessionDetail => {
                let sel = self.state.detail_selected;
                if sel < self.state.detail_offset {
                    self.state.detail_offset = sel;
                } else if sel >= self.state.detail_offset + h {
                    self.state.detail_offset = sel - h + 1;
                }
            }
        }
    }
}

/// Entry point for `steve data` — opens DB read-only and runs the data browser TUI.
pub fn run(db_path: &Path) -> Result<()> {
    // Check if DB exists
    if !db_path.exists() {
        eprintln!(
            "No usage data yet.\n\
             Start a conversation with `steve` to begin recording usage.\n\
             Expected database at: {}",
            db_path.display()
        );
        return Ok(());
    }

    let conn = db::open_readonly(db_path)?;
    let mut app = DataApp::new(conn);
    app.load_data()?;

    if app.state.sessions.is_empty() {
        eprintln!("No sessions recorded yet. Start a conversation with `steve` to begin.");
        return Ok(());
    }

    // Set up terminal
    let mut terminal = crate::ui::setup_terminal()?;

    let result = run_event_loop(&mut terminal, &mut app);

    // Always restore terminal
    crate::ui::restore_terminal(&mut terminal)?;

    result
}

/// Synchronous event loop — poll at 100ms, render, handle key events.
fn run_event_loop(terminal: &mut crate::ui::Tui, app: &mut DataApp) -> Result<()> {
    loop {
        // Render
        terminal.draw(|frame| {
            app.update_scroll(frame.area().height);
            views::render(frame, &app.state, &app.theme);
        })?;

        if app.should_quit {
            break;
        }

        // Poll for events
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            app.handle_key(key.code, key.modifiers)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{
        db::open_in_memory,
        types::{ApiCallRecord, ProjectRecord, SessionRecord},
    };
    use chrono::Utc;
    use state::SortColumn;

    fn make_test_app() -> DataApp {
        let conn = open_in_memory().unwrap();

        // Seed test data
        db::upsert_project(
            &conn,
            &ProjectRecord {
                project_id: "proj-1".into(),
                display_name: "Test Project".into(),
                root_path: "/tmp/test".into(),
            },
        )
        .unwrap();

        db::upsert_session(
            &conn,
            &SessionRecord {
                session_id: "sess-1".into(),
                project_id: "proj-1".into(),
                title: "Fix permission bug".into(),
                model_ref: "openai/gpt-4o".into(),
                created_at: Utc::now(),
            },
        )
        .unwrap();

        for i in 0..3 {
            db::insert_api_call(
                &conn,
                &ApiCallRecord {
                    timestamp: Utc::now(),
                    project_id: "proj-1".into(),
                    session_id: "sess-1".into(),
                    model_ref: "openai/gpt-4o".into(),
                    prompt_tokens: 1000 + i * 500,
                    completion_tokens: 200 + i * 50,
                    total_tokens: 1200 + i * 550,
                    cost: Some(0.01 * (i + 1) as f64),
                    duration_ms: 800 + i as u64 * 200,
                    iteration: i,
                },
            )
            .unwrap();
        }

        let mut app = DataApp::new(conn);
        app.load_data().unwrap();
        app
    }

    #[test]
    fn load_data_populates_state() {
        let app = make_test_app();
        assert_eq!(app.state.sessions.len(), 1);
        assert_eq!(app.state.sessions[0].call_count, 3);
        assert_eq!(app.state.projects.len(), 1);
        assert_eq!(app.state.stats.session_count, 1);
        assert_eq!(app.state.stats.call_count, 3);
    }

    #[test]
    fn navigate_down_and_up() {
        let mut app = make_test_app();
        assert_eq!(app.state.selected_row, 0);

        // Can't go below 0 with only 1 session
        app.handle_key(KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.selected_row, 0);

        app.handle_key(KeyCode::Up, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.selected_row, 0);
    }

    #[test]
    fn enter_drills_into_detail() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.view, View::SessionDetail);
        assert_eq!(app.state.detail_calls.len(), 3);
        assert_eq!(app.state.detail_session_title, "Fix permission bug");
    }

    #[test]
    fn esc_returns_from_detail() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.view, View::SessionDetail);

        app.handle_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.view, View::SessionList);
    }

    #[test]
    fn tab_cycles_sort_column() {
        let mut app = make_test_app();
        assert_eq!(app.state.sort_column, SortColumn::Date);

        app.handle_key(KeyCode::Tab, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.sort_column, SortColumn::Title);

        app.handle_key(KeyCode::Tab, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.sort_column, SortColumn::Model);
    }

    #[test]
    fn r_reverses_sort() {
        let mut app = make_test_app();
        assert!(!app.state.sort_ascending);

        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE)
            .unwrap();
        assert!(app.state.sort_ascending);
    }

    #[test]
    fn q_quits() {
        let mut app = make_test_app();
        assert!(!app.should_quit);

        app.handle_key(KeyCode::Char('q'), KeyModifiers::NONE)
            .unwrap();
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL)
            .unwrap();
        assert!(app.should_quit);
    }

    #[test]
    fn f_toggles_filter() {
        let mut app = make_test_app();
        assert!(!app.state.filter_active);

        app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE)
            .unwrap();
        assert!(app.state.filter_active);
        assert_eq!(app.state.filter_field, FilterField::Project);
    }

    #[test]
    fn filter_cycling() {
        let mut app = make_test_app();
        app.state.filter_active = true;
        app.state.filter_field = FilterField::Project;

        // Cycle right into first project
        app.handle_filter_key(KeyCode::Right).unwrap();
        assert_eq!(app.state.filter_project_idx, Some(0));

        // Cycle right again wraps to None (All)
        app.handle_filter_key(KeyCode::Right).unwrap();
        assert_eq!(app.state.filter_project_idx, None);

        // Tab switches to model field
        app.handle_filter_key(KeyCode::Tab).unwrap();
        assert_eq!(app.state.filter_field, FilterField::Model);
    }

    #[test]
    fn filter_apply_and_cancel() {
        let mut app = make_test_app();
        app.state.filter_active = true;

        // Esc cancels
        app.handle_filter_key(KeyCode::Esc).unwrap();
        assert!(!app.state.filter_active);

        // Enter applies
        app.state.filter_active = true;
        app.handle_filter_key(KeyCode::Enter).unwrap();
        assert!(!app.state.filter_active);
    }

    #[test]
    fn detail_navigation() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.detail_selected, 0);

        app.handle_key(KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.detail_selected, 1);

        app.handle_key(KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.detail_selected, 2);

        // Can't go past last
        app.handle_key(KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.detail_selected, 2);

        app.handle_key(KeyCode::Up, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.detail_selected, 1);
    }

    #[test]
    fn backspace_returns_from_detail() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert_eq!(app.state.view, View::SessionDetail);

        app.handle_key(KeyCode::Backspace, KeyModifiers::NONE)
            .unwrap();
        assert_eq!(app.state.view, View::SessionList);
    }

    #[test]
    fn j_k_navigation() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();

        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE)
            .unwrap();
        assert_eq!(app.state.detail_selected, 1);

        app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE)
            .unwrap();
        assert_eq!(app.state.detail_selected, 0);
    }

    #[test]
    fn empty_db_no_crash() {
        let conn = open_in_memory().unwrap();
        let mut app = DataApp::new(conn);
        app.load_data().unwrap();
        assert!(app.state.sessions.is_empty());
        assert_eq!(app.state.stats.session_count, 0);
    }

    #[test]
    fn render_session_list_no_crash() {
        let app = make_test_app();
        let _buf = crate::ui::render_to_buffer(80, 24, |frame| {
            views::render(frame, &app.state, &app.theme);
        });
    }

    #[test]
    fn render_detail_view_no_crash() {
        let mut app = make_test_app();
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();

        let _buf = crate::ui::render_to_buffer(80, 24, |frame| {
            views::render(frame, &app.state, &app.theme);
        });
    }

    #[test]
    fn render_empty_state_no_crash() {
        let conn = open_in_memory().unwrap();
        let mut app = DataApp::new(conn);
        app.load_data().unwrap();

        let _buf = crate::ui::render_to_buffer(80, 24, |frame| {
            views::render(frame, &app.state, &app.theme);
        });
    }

    #[test]
    fn render_with_filter_active() {
        let mut app = make_test_app();
        app.state.filter_active = true;

        let _buf = crate::ui::render_to_buffer(80, 24, |frame| {
            views::render(frame, &app.state, &app.theme);
        });
    }
}
