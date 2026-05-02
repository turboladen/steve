//! Eval harness — scenario-based regression testing for Steve's chat/coding quality.
//!
//! Each scenario describes a setup (fixture files + shell commands), a sequence of user
//! turns, and a list of expectations to check against the captured run. Scoring is hybrid:
//! rule-based assertions handle structural facts (tool-call sequence, file diffs); a small
//! LLM-as-judge handles behavioral checks ("did the assistant surrender?", "did it
//! acknowledge the constraint?") where idiom drift makes regex matching brittle.
//!
//! See `docs/superpowers/specs/2026-05-02-eval-harness-design.md` (or the plan file) for the
//! full design.
//!
//! Build phases (tracked as separate beads issues under epic steve-ffdq):
//! - Phase 1: scenario format + parser — `scenario.rs`  (this commit)
//! - Phase 2: headless App driver — `runner.rs`
//! - Phase 3: rule-based assertions — `expectations.rs`
//! - Phase 4: LLM-as-judge — `judge.rs`
//! - Phase 5: initial 10 scenarios under `eval/scenarios/`
//! - Phase 6: JSONL output + `compare` — `report.rs`
//! - Phase 7: scenario-from-debug generator — `from_debug.rs`

pub mod scenario;

pub use scenario::{Expectation, Scenario, Setup};
