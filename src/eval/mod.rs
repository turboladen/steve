//! Eval harness — scenario-based regression testing for Steve's chat/coding quality.
//!
//! Each scenario describes a setup (fixture files + shell commands), a sequence of user
//! turns, and a list of expectations to check against the captured run. Scoring is hybrid:
//! rule-based assertions handle structural facts (tool-call sequence, file diffs); a small
//! LLM-as-judge handles behavioral checks where idiom drift makes regex matching brittle.

pub mod baseline;
pub mod capture;
pub mod cli;
pub mod expectations;
pub mod history;
pub mod html_report;
pub mod judge;
pub mod report;
pub mod results;
pub mod runner;
pub mod scenario;
pub mod score;
pub mod transcript;
pub mod workspace;

pub use baseline::{BaselineFile, Manifest, ManifestEntry, baseline_path, manifest_path};
pub use capture::{CapturedRun, RecordedToolCall};
pub use expectations::{EvalReport, ExpectationResult, JudgeRecord, Outcome, evaluate};
pub use history::{HistoryEntry, append_history, read_history};
pub use html_report::render_html;
pub use judge::{ComparePair, Judge, JudgeAdapter, JudgeOutcome, JudgeVerdict, apply_judges};
pub use report::{
    AxisTotals, BaselineProvenance, Report, ReportTotals, ScenarioOutcome, ScenarioReport,
};
pub use results::{ResultsFile, ScenarioResults};
pub use runner::Runner;
pub use scenario::{Expectation, Scenario, Scoring, Setup, discover_scenarios};
pub use score::{Axis, CompareVerdict, DEFAULT_AXES, PairedScore, ScenarioScore, Verdict};
pub use transcript::{NormalizedTranscript, Normalizer, TranscriptEvent, UsageSummary};
pub use workspace::{ScenarioWorkspace, WorkspaceSnapshot};
