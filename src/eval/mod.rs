//! Eval harness — scenario-based regression testing for Steve's chat/coding quality.
//!
//! Each scenario describes a setup (fixture files + shell commands), a sequence of user
//! turns, and a list of expectations to check against the captured run. Scoring is hybrid:
//! rule-based assertions handle structural facts (tool-call sequence, file diffs); a small
//! LLM-as-judge handles behavioral checks where idiom drift makes regex matching brittle.

pub mod capture;
pub mod runner;
pub mod scenario;
pub mod workspace;

pub use capture::{CapturedRun, RecordedToolCall};
pub use runner::Runner;
pub use scenario::{Expectation, Scenario, Setup};
pub use workspace::{ScenarioWorkspace, WorkspaceSnapshot};
