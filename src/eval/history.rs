//! Append-only JSONL history at `eval/history.jsonl`. One row per
//! recorded `steve eval report --record-history` invocation. Bare
//! `report` is read-only against the file.
//!
//! Schema per row matches spec lines 600-614:
//! `git_ref` + `recorded_at` + `model` + `baseline_git_ref` +
//! `judge_model` + `headline` + `per_axis` + `deterministic_floor`
//! + `results_file`.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::eval::report::Report;

/// One row of the history file. Serializes as a single line of JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub git_ref: String,
    pub recorded_at: String,
    pub model: String,
    pub baseline_git_ref: String,
    pub judge_model: String,
    pub headline: HistoryHeadline,
    pub per_axis: BTreeMap<String, HistoryAxisEntry>,
    pub deterministic_floor: HistoryFloor,
    pub results_file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryHeadline {
    pub net_win_rate: f64,
    pub non_regression_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryAxisEntry {
    pub net_win_rate: f64,
    pub won: usize,
    pub lost: usize,
    pub tied: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryFloor {
    pub passed: usize,
    pub total: usize,
}

impl HistoryEntry {
    /// Build a history row from a populated `Report`. `recorded_at`
    /// is passed separately so the caller controls the timestamp
    /// (typically `chrono::Utc::now()` formatted as RFC 3339).
    pub fn from_report(report: &Report, recorded_at: String) -> Self {
        // Pick a representative baseline git_ref. Spec assumes a
        // single anchor per report; if multiple appear (one per
        // scenario), use the first one (the renderer surfaces the
        // divergence in --verbose).
        let baseline_git_ref = report
            .baseline_provenance
            .values()
            .next()
            .map(|p| p.git_ref.clone())
            .unwrap_or_else(|| "unknown".into());
        let per_axis = report
            .per_axis
            .iter()
            .map(|ax| {
                (
                    format!("{}", ax.axis),
                    HistoryAxisEntry {
                        net_win_rate: ax.totals.net_win_rate(),
                        won: ax.totals.current_wins,
                        lost: ax.totals.baseline_wins,
                        tied: ax.totals.ties,
                    },
                )
            })
            .collect();
        HistoryEntry {
            git_ref: report.results_git_ref.clone(),
            recorded_at,
            model: report.model.clone(),
            baseline_git_ref,
            judge_model: report.judge_model.clone(),
            headline: HistoryHeadline {
                net_win_rate: report.headline_totals.net_win_rate(),
                non_regression_rate: report.headline_totals.non_regression_rate(),
            },
            per_axis,
            // Deterministic floor info comes from the legacy assertion
            // channel; not yet plumbed through to Report. Spec lists
            // it as a field; we ship with passed=total=0 for now and
            // surface the gap in a follow-up issue.
            deterministic_floor: HistoryFloor {
                passed: 0,
                total: 0,
            },
            results_file: report.results_path.clone(),
        }
    }
}

/// Append one `HistoryEntry` as a single line to `path`. Creates the
/// parent directory and the file if absent. Each row is exactly one
/// line of compact JSON (no pretty-printing — JSONL contract).
pub fn append_history(path: &Path, entry: &HistoryEntry) -> Result<()> {
    use std::{fs::OpenOptions, io::Write};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir for {}", path.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for append", path.display()))?;
    let line = serde_json::to_string(entry).context("serializing history entry")?;
    writeln!(file, "{line}")
        .with_context(|| format!("writing history row to {}", path.display()))?;
    Ok(())
}

/// Read every row in `path`, one per line. Returns an empty Vec if
/// the file doesn't exist (no rows yet is not an error). Malformed
/// rows propagate as Err so corrupt history is loud rather than
/// silently dropping data.
pub fn read_history(path: &Path) -> Result<Vec<HistoryEntry>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(anyhow::Error::from(e).context(format!("reading {}", path.display())));
        }
    };
    let mut out = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: HistoryEntry = serde_json::from_str(line)
            .with_context(|| format!("parsing history row {} in {}", i + 1, path.display()))?;
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::report::{BaselineProvenance, Report, ReportTotals};
    use tempfile::TempDir;

    fn sample_entry() -> HistoryEntry {
        HistoryEntry {
            git_ref: "def5678".into(),
            recorded_at: "2026-05-12T14:23:00Z".into(),
            model: "ollama/qwen3-coder".into(),
            baseline_git_ref: "abc1234".into(),
            judge_model: "fuel-ix/claude-haiku-4-5".into(),
            headline: HistoryHeadline {
                net_win_rate: 0.022,
                non_regression_rate: 0.978,
            },
            per_axis: {
                let mut m = BTreeMap::new();
                m.insert(
                    "correctness".into(),
                    HistoryAxisEntry {
                        net_win_rate: -0.033,
                        won: 1,
                        lost: 2,
                        tied: 27,
                    },
                );
                m
            },
            deterministic_floor: HistoryFloor {
                passed: 10,
                total: 10,
            },
            results_file: "path/to/results.yaml".into(),
        }
    }

    #[test]
    fn append_then_read_round_trips() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");
        let e = sample_entry();
        append_history(&path, &e).unwrap();
        let rows = read_history(&path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], e);
    }

    #[test]
    fn multiple_appends_preserve_order() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");
        let mut e1 = sample_entry();
        e1.git_ref = "first".into();
        let mut e2 = sample_entry();
        e2.git_ref = "second".into();
        append_history(&path, &e1).unwrap();
        append_history(&path, &e2).unwrap();
        let rows = read_history(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].git_ref, "first");
        assert_eq!(rows[1].git_ref, "second");
    }

    #[test]
    fn read_missing_file_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.jsonl");
        let rows = read_history(&path).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn each_appended_row_is_single_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");
        append_history(&path, &sample_entry()).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        // Exactly one newline at the end of the only row.
        assert_eq!(raw.matches('\n').count(), 1);
        // No embedded newlines from pretty-printing.
        let body = raw.trim_end();
        assert!(
            !body.contains('\n'),
            "JSON row must be single-line; got: {body:?}"
        );
    }

    #[test]
    fn entry_from_report_extracts_spec_fields() {
        let r = Report {
            model: "ollama/qwen3-coder".into(),
            results_git_ref: "def5678".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: {
                let mut m = BTreeMap::new();
                m.insert(
                    "_smoke".into(),
                    BaselineProvenance {
                        git_ref: "abc1234".into(),
                        frozen_at: "2026-05-01T00:00:00Z".into(),
                    },
                );
                m
            },
            judge_model: "fuel-ix/claude-haiku-4-5".into(),
            headline_totals: ReportTotals {
                current_wins: 1,
                baseline_wins: 0,
                ties: 9,
            },
            per_axis: Vec::new(),
            scenarios: Vec::new(),
        };
        let entry = HistoryEntry::from_report(&r, "2026-05-12T14:23:00Z".into());
        assert_eq!(entry.git_ref, "def5678");
        assert_eq!(entry.model, "ollama/qwen3-coder");
        assert_eq!(entry.baseline_git_ref, "abc1234");
        assert_eq!(entry.judge_model, "fuel-ix/claude-haiku-4-5");
        assert!((entry.headline.net_win_rate - 0.1).abs() < 1e-9);
    }

    #[test]
    fn malformed_row_propagates_as_err() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");
        std::fs::write(&path, "not valid json\n").unwrap();
        let result = read_history(&path);
        assert!(result.is_err(), "malformed row must surface as Err");
    }
}
