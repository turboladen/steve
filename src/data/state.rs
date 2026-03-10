use crate::usage::types::{
    ApiCallDetail, ProjectInfo, SessionFilter, SessionSummary, UsageStats,
};

/// Which view is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    SessionList,
    SessionDetail,
}

/// Which column the session list is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Date,
    Title,
    Model,
    Tokens,
    Cost,
}

impl SortColumn {
    /// All columns in display order.
    pub const ALL: [SortColumn; 5] = [
        SortColumn::Date,
        SortColumn::Title,
        SortColumn::Model,
        SortColumn::Tokens,
        SortColumn::Cost,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SortColumn::Date => "Date",
            SortColumn::Title => "Title",
            SortColumn::Model => "Model",
            SortColumn::Tokens => "Tokens",
            SortColumn::Cost => "Cost",
        }
    }

    /// Advance to next column (wraps).
    pub fn next(&self) -> SortColumn {
        let all = Self::ALL;
        let idx = all.iter().position(|c| c == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }

    /// Advance to previous column (wraps).
    pub fn prev(&self) -> SortColumn {
        let all = Self::ALL;
        let idx = all.iter().position(|c| c == self).unwrap_or(0);
        all[(idx + all.len() - 1) % all.len()]
    }
}

/// All state for the data TUI.
pub struct DataState {
    pub view: View,
    pub sessions: Vec<SessionSummary>,
    pub selected_row: usize,
    pub sort_column: SortColumn,
    pub sort_ascending: bool,
    pub stats: UsageStats,
    pub filter: SessionFilter,
    pub filter_active: bool,
    pub filter_field: FilterField,
    pub projects: Vec<ProjectInfo>,
    pub models: Vec<String>,
    pub filter_project_idx: Option<usize>,
    pub filter_model_idx: Option<usize>,

    // Detail view state
    pub detail_calls: Vec<ApiCallDetail>,
    pub detail_selected: usize,
    pub detail_session_title: String,
    pub detail_session_model: String,
    pub detail_session_date: String,

    // Scroll offset for session list (for large lists)
    pub list_offset: usize,
    pub detail_offset: usize,
}

impl DataState {
    pub fn new() -> Self {
        Self {
            view: View::SessionList,
            sessions: Vec::new(),
            selected_row: 0,
            sort_column: SortColumn::Date,
            sort_ascending: false,
            stats: UsageStats::default(),
            filter: SessionFilter::default(),
            filter_active: false,
            filter_field: FilterField::Project,
            projects: Vec::new(),
            models: Vec::new(),
            filter_project_idx: None,
            filter_model_idx: None,
            detail_calls: Vec::new(),
            detail_selected: 0,
            detail_session_title: String::new(),
            detail_session_model: String::new(),
            detail_session_date: String::new(),
            list_offset: 0,
            detail_offset: 0,
        }
    }

    /// Sort sessions in memory by the current column + direction.
    pub fn sort_sessions(&mut self) {
        let asc = self.sort_ascending;
        self.sessions.sort_by(|a, b| {
            let ord = match self.sort_column {
                SortColumn::Date => a.created_at.cmp(&b.created_at),
                SortColumn::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                SortColumn::Model => a.model_ref.cmp(&b.model_ref),
                SortColumn::Tokens => a.total_tokens.cmp(&b.total_tokens),
                SortColumn::Cost => {
                    let ac = a.total_cost.unwrap_or(0.0);
                    let bc = b.total_cost.unwrap_or(0.0);
                    ac.partial_cmp(&bc).unwrap_or(std::cmp::Ordering::Equal)
                }
            };
            if asc { ord } else { ord.reverse() }
        });
    }

    /// Ensure selected_row doesn't exceed bounds.
    pub fn clamp_selection(&mut self) {
        let len = match self.view {
            View::SessionList => self.sessions.len(),
            View::SessionDetail => self.detail_calls.len(),
        };
        if len == 0 {
            self.set_selected(0);
        } else {
            let sel = self.selected();
            if sel >= len {
                self.set_selected(len - 1);
            }
        }
    }

    pub fn selected(&self) -> usize {
        match self.view {
            View::SessionList => self.selected_row,
            View::SessionDetail => self.detail_selected,
        }
    }

    pub fn set_selected(&mut self, idx: usize) {
        match self.view {
            View::SessionList => self.selected_row = idx,
            View::SessionDetail => self.detail_selected = idx,
        }
    }
}

/// Which filter field is active in the filter bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterField {
    Project,
    Model,
}

impl FilterField {
    pub fn next(&self) -> FilterField {
        match self {
            FilterField::Project => FilterField::Model,
            FilterField::Model => FilterField::Project,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_column_next_wraps() {
        assert_eq!(SortColumn::Date.next(), SortColumn::Title);
        assert_eq!(SortColumn::Cost.next(), SortColumn::Date);
    }

    #[test]
    fn sort_column_prev_wraps() {
        assert_eq!(SortColumn::Date.prev(), SortColumn::Cost);
        assert_eq!(SortColumn::Title.prev(), SortColumn::Date);
    }

    #[test]
    fn sort_column_all_labels() {
        for col in SortColumn::ALL {
            assert!(!col.label().is_empty());
        }
    }

    #[test]
    fn sort_column_all_is_complete() {
        // Safety: if a new variant is added to SortColumn, this match will fail to compile,
        // reminding the author to add it to ALL.
        for col in &SortColumn::ALL {
            match col {
                SortColumn::Date
                | SortColumn::Title
                | SortColumn::Model
                | SortColumn::Tokens
                | SortColumn::Cost => {}
            }
        }
        // If ALL were missing a variant, next()/prev() would skip it.
        // Verify round-trip: cycling ALL.len() times returns to start.
        let start = SortColumn::Date;
        let mut current = start;
        for _ in 0..SortColumn::ALL.len() {
            current = current.next();
        }
        assert_eq!(current, start, "next() round-trip should return to start");
    }

    #[test]
    fn filter_field_toggle() {
        assert_eq!(FilterField::Project.next(), FilterField::Model);
        assert_eq!(FilterField::Model.next(), FilterField::Project);
    }

    #[test]
    fn data_state_default_is_session_list() {
        let state = DataState::new();
        assert_eq!(state.view, View::SessionList);
        assert_eq!(state.sort_column, SortColumn::Date);
        assert!(!state.sort_ascending);
    }

    #[test]
    fn sort_sessions_by_tokens() {
        let mut state = DataState::new();
        state.sessions = vec![
            SessionSummary {
                session_id: "a".into(),
                project_id: "p".into(),
                project_name: "P".into(),
                title: "Low".into(),
                model_ref: "m".into(),
                created_at: "2026-01-01".into(),
                call_count: 1,
                total_prompt_tokens: 100,
                total_completion_tokens: 50,
                total_tokens: 150,
                total_cost: Some(0.01),
                total_duration_ms: 500,
            },
            SessionSummary {
                session_id: "b".into(),
                project_id: "p".into(),
                project_name: "P".into(),
                title: "High".into(),
                model_ref: "m".into(),
                created_at: "2026-01-02".into(),
                call_count: 5,
                total_prompt_tokens: 5000,
                total_completion_tokens: 1000,
                total_tokens: 6000,
                total_cost: Some(0.50),
                total_duration_ms: 3000,
            },
        ];

        state.sort_column = SortColumn::Tokens;
        state.sort_ascending = false;
        state.sort_sessions();
        assert_eq!(state.sessions[0].session_id, "b"); // highest first

        state.sort_ascending = true;
        state.sort_sessions();
        assert_eq!(state.sessions[0].session_id, "a"); // lowest first
    }

    #[test]
    fn clamp_selection_on_empty() {
        let mut state = DataState::new();
        state.selected_row = 5;
        state.clamp_selection();
        assert_eq!(state.selected_row, 0);
    }
}
