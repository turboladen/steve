//! Aggregation of paired-comparison verdicts into the layered
//! Phase 8 report — headline + per-axis + per-scenario detail.
//!
//! Pure data types and pure formula methods. No I/O. The
//! orchestration that loads `ResultsFile` + baselines and calls
//! `Judge::compare` to populate a `Report` lives in `cli.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::eval::score::{Axis, PairedScore, Verdict};

/// Suite-wide or per-slice tally of `CurrentWins`, `BaselineWins`,
/// and `Tie` verdicts. The three formulas in the spec all operate
/// on this primitive shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTotals {
    pub current_wins: usize,
    pub baseline_wins: usize,
    pub ties: usize,
}

impl ReportTotals {
    pub fn total(&self) -> usize {
        self.current_wins + self.baseline_wins + self.ties
    }

    /// Net win rate, `(W - L) / (W + L + T)`. Range `[-1.0, +1.0]`.
    /// Returns `0.0` when the total is zero (no verdicts) — the
    /// spec's "all ties" reading collapses to the same value, and
    /// dividing by zero would be worse than reporting "no change".
    pub fn net_win_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        (self.current_wins as f64 - self.baseline_wins as f64) / total as f64
    }

    /// Non-regression rate, `(W + T) / (W + L + T)`. Range
    /// `[0.0, 1.0]`. Returns `1.0` when no verdicts (vacuously true).
    pub fn non_regression_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 1.0;
        }
        (self.current_wins + self.ties) as f64 / total as f64
    }

    /// Fold a single `Verdict` into the totals. Used during
    /// aggregation to walk every cell in axis order.
    pub fn add(&mut self, verdict: Verdict) {
        match verdict {
            Verdict::CurrentWins => self.current_wins += 1,
            Verdict::BaselineWins => self.baseline_wins += 1,
            Verdict::Tie => self.ties += 1,
        }
    }
}

/// Per-axis tally + the axis identity. The full per-axis section of
/// the layered report is a `Vec<AxisTotals>` in spec-axis order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisTotals {
    pub axis: Axis,
    pub totals: ReportTotals,
}

/// Why a scenario contributes (or doesn't) to aggregate totals.
/// `Graded` means every run produced verdicts that got bucketed
/// into the totals; `Skipped` means we never called the judge
/// (missing baseline) and the scenario is reported separately in
/// the headline's "Skipped:" subsection. All-runs-errored on a
/// scenario maps to `Skipped` per spec ("if ALL K runs of a
/// scenario error, the scenario is treated like a missing baseline").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioOutcome {
    Graded {
        /// One `Vec<PairedScore>` per run; length matches the count
        /// of runs that successfully graded. Runs that errored on
        /// both attempts are excluded from this list (and surface
        /// in `--verbose` output via a separate channel — see
        /// the renderer).
        per_run_scores: Vec<Vec<PairedScore>>,
        /// Per-axis tally for this scenario alone, for the verbose
        /// per-scenario rendering.
        per_axis: Vec<AxisTotals>,
    },
    Skipped {
        /// Human-readable reason — the renderer prints this in the
        /// "Skipped:" subsection of the headline. Typical values:
        /// `"no baseline for scenario X with model Y: run …"` or
        /// `"all K runs of scenario X errored: <last error msg>"`.
        reason: String,
    },
}

/// Per-scenario record carried by `Report.scenarios`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub scenario: String,
    pub outcome: ScenarioOutcome,
}

/// Per-scenario baseline provenance fields surfaced in the report's
/// metadata block. Both come from the baseline manifest entry; the
/// renderer uses them to print "frozen YYYY-MM-DD at <ref>" alongside
/// the headline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineProvenance {
    pub git_ref: String,
    pub frozen_at: String,
}

/// The complete result of a `steve eval report` run. Pure data;
/// rendering and exit-code computation hang off this.
///
/// `headline_totals` is the suite-wide tally across all
/// `S_graded × K × A` cells (where `S_graded` excludes Skipped
/// scenarios). `per_axis` is the same tally sliced by axis.
/// `scenarios` is the per-scenario detail used by `--verbose`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Model the results were sampled from (e.g., `"ollama/qwen3-coder"`).
    pub model: String,
    /// Git ref recorded in the results file.
    pub results_git_ref: String,
    /// Path to the results file (for provenance + `history.jsonl`).
    pub results_path: String,
    /// Per-scenario baseline provenance. Keys are scenario names.
    /// Missing entries indicate the scenario was skipped (no baseline).
    pub baseline_provenance: BTreeMap<String, BaselineProvenance>,
    /// Judge model used for this report (resolved precedence:
    /// CLI > scenario.judge_model > error).
    pub judge_model: String,
    /// Suite-wide tally across all (scenario × run × axis) cells
    /// that were graded.
    pub headline_totals: ReportTotals,
    /// Per-axis slice of `headline_totals`. Order is spec-axis order
    /// (typically Correctness, Efficiency, Conciseness, but may differ
    /// when scenarios override `[scoring].axes`).
    pub per_axis: Vec<AxisTotals>,
    /// Per-scenario detail. Order matches results-file insertion order.
    pub scenarios: Vec<ScenarioReport>,
}

impl Report {
    /// Build a `Report` by walking every (scenario, run) pair in
    /// `results`, resolving a baseline from `baselines_dir`, and
    /// calling `judge.compare(...)` for each cell. Missing baselines
    /// surface as `Skipped`. Transient judge errors are retried once;
    /// double-failures exclude the run from `per_run_scores`.
    /// All-runs-errored on a scenario maps to `Skipped` per spec.
    ///
    /// `scenarios_dir` is `Option` because Phase-8 callers always pass
    /// `Some(eval/scenarios)` to read per-scenario `[scoring].axes` and
    /// `judge_model` overrides, but unit tests pass `None` to keep the
    /// fake judge focused on orchestration logic (axes default to
    /// `DEFAULT_AXES`, scenario_judge_model defaults to `None`).
    pub async fn build_from_results(
        results: &crate::eval::results::ResultsFile,
        baselines_dir: &std::path::Path,
        results_path: &str,
        judge: &dyn crate::eval::JudgeAdapter,
        judge_model: &str,
        scenarios_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        use crate::eval::{
            baseline::{BaselineFile, baseline_path},
            scenario::Scenario,
        };

        let mut report = Report {
            model: results.model.clone(),
            results_git_ref: results.git_ref.clone(),
            results_path: results_path.to_string(),
            baseline_provenance: BTreeMap::new(),
            judge_model: judge_model.to_string(),
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: Vec::new(),
        };

        // Accumulate per-axis tallies; finalize ordering below.
        let mut per_axis_map: BTreeMap<Axis, ReportTotals> = BTreeMap::new();

        for (scenario_name, scenario_results) in &results.scenarios {
            let bpath = match baseline_path(baselines_dir, scenario_name, &results.model) {
                Ok(p) => p,
                Err(e) => {
                    report.scenarios.push(ScenarioReport {
                        scenario: scenario_name.clone(),
                        outcome: ScenarioOutcome::Skipped {
                            reason: format!("baseline path resolution failed: {e:#}"),
                        },
                    });
                    continue;
                }
            };
            if !bpath.exists() {
                report.scenarios.push(ScenarioReport {
                    scenario: scenario_name.clone(),
                    outcome: ScenarioOutcome::Skipped {
                        reason: format!(
                            "no baseline for scenario '{scenario_name}' with model '{}': \
                             run `steve eval baseline freeze --scenario {scenario_name} \
                             --model {}`",
                            results.model, results.model
                        ),
                    },
                });
                continue;
            }
            let baseline = match BaselineFile::read_from_path(&bpath) {
                Ok(b) => b,
                Err(e) => {
                    report.scenarios.push(ScenarioReport {
                        scenario: scenario_name.clone(),
                        outcome: ScenarioOutcome::Skipped {
                            reason: format!("baseline load failed: {e:#}"),
                        },
                    });
                    continue;
                }
            };
            report.baseline_provenance.insert(
                scenario_name.clone(),
                BaselineProvenance {
                    git_ref: baseline.git_ref.clone(),
                    frozen_at: baseline.frozen_at.clone(),
                },
            );

            // Determine axes + per-scenario judge model from the
            // on-disk scenario.toml when `scenarios_dir` was supplied.
            let scenario_on_disk = scenarios_dir.and_then(|dir| {
                Scenario::from_file(&dir.join(scenario_name).join("scenario.toml")).ok()
            });
            let axes: Vec<Axis> = match &scenario_on_disk {
                Some(scn) => scn.scoring_axes().to_vec(),
                None => crate::eval::score::DEFAULT_AXES.to_vec(),
            };
            let scenario_judge_model: Option<String> = scenario_on_disk
                .as_ref()
                .and_then(|scn| scn.judge_model.clone());

            // Walk each run; call judge with retry-once.
            let mut per_run_scores: Vec<Vec<PairedScore>> =
                Vec::with_capacity(scenario_results.runs.len());
            let mut last_error: Option<String> = None;
            for current_transcript in &scenario_results.runs {
                let pair = crate::eval::judge::ComparePair {
                    baseline: &baseline.transcript,
                    current: current_transcript,
                };
                let mut attempts = 0;
                let scores = loop {
                    attempts += 1;
                    match judge
                        .compare(
                            pair,
                            &axes,
                            &scenario_results.user_turns,
                            scenario_judge_model.as_deref(),
                        )
                        .await
                    {
                        Ok(s) => break Some(s),
                        Err(e) => {
                            if attempts >= 2 {
                                last_error = Some(format!("{e:#}"));
                                break None;
                            }
                        }
                    }
                };
                if let Some(s) = scores {
                    per_run_scores.push(s);
                }
            }

            if per_run_scores.is_empty() {
                report.scenarios.push(ScenarioReport {
                    scenario: scenario_name.clone(),
                    outcome: ScenarioOutcome::Skipped {
                        reason: format!(
                            "all {} run(s) of scenario '{scenario_name}' errored: {}",
                            scenario_results.runs.len(),
                            last_error.unwrap_or_else(|| "unknown".into())
                        ),
                    },
                });
                continue;
            }

            // Bucket verdicts into per-axis + headline + per-scenario.
            let mut scenario_per_axis: BTreeMap<Axis, ReportTotals> = BTreeMap::new();
            for run_scores in &per_run_scores {
                for score in run_scores {
                    report.headline_totals.add(score.verdict);
                    per_axis_map
                        .entry(score.axis)
                        .or_default()
                        .add(score.verdict);
                    scenario_per_axis
                        .entry(score.axis)
                        .or_default()
                        .add(score.verdict);
                }
            }
            // Convert scenario_per_axis to ordered Vec<AxisTotals>
            // following the requested axes order:
            let scenario_per_axis_vec: Vec<AxisTotals> = axes
                .iter()
                .filter_map(|a| {
                    scenario_per_axis.get(a).map(|t| AxisTotals {
                        axis: *a,
                        totals: *t,
                    })
                })
                .collect();
            report.scenarios.push(ScenarioReport {
                scenario: scenario_name.clone(),
                outcome: ScenarioOutcome::Graded {
                    per_run_scores,
                    per_axis: scenario_per_axis_vec,
                },
            });
        }

        // Finalize per_axis: DEFAULT_AXES order first, then any
        // remaining (override-only) axes in BTreeMap order.
        for axis in crate::eval::score::DEFAULT_AXES {
            if let Some(t) = per_axis_map.remove(&axis) {
                report.per_axis.push(AxisTotals { axis, totals: t });
            }
        }
        for (axis, t) in per_axis_map {
            report.per_axis.push(AxisTotals { axis, totals: t });
        }

        Ok(report)
    }

    /// Render the layered text report. `verbose` enables the
    /// per-scenario section.
    pub fn render_text(&self, verbose: bool) -> String {
        let mut out = String::new();

        // Metadata block.
        out.push_str(&format!(
            "Eval results — current ({} at {}) vs baseline\n",
            self.model, self.results_git_ref
        ));
        if let Some(prov) = self.baseline_provenance.values().next() {
            // If all baselines are from the same git_ref, show it
            // once. Otherwise note the divergence here and surface
            // per-scenario details in --verbose.
            let single_ref = self
                .baseline_provenance
                .values()
                .all(|p| p.git_ref == prov.git_ref);
            if single_ref {
                out.push_str(&format!(
                    "  baseline frozen {} at {} ({} scenarios)\n\n",
                    prov.frozen_at,
                    prov.git_ref,
                    self.baseline_provenance.len()
                ));
            } else {
                out.push_str(&format!(
                    "  baselines from {} scenarios (varied refs — see --verbose)\n\n",
                    self.baseline_provenance.len()
                ));
            }
        } else {
            out.push('\n');
        }

        // Headline.
        out.push_str("  Headline:        ");
        out.push_str(&format!(
            "{} net win rate ({:.1}% non-regression)\n",
            format_signed_percent(self.headline_totals.net_win_rate()),
            self.headline_totals.non_regression_rate() * 100.0
        ));

        // Skipped section (between headline and per-axis, so it's
        // immediately visible without scrolling past the axes).
        let skipped: Vec<&ScenarioReport> = self
            .scenarios
            .iter()
            .filter(|s| matches!(s.outcome, ScenarioOutcome::Skipped { .. }))
            .collect();
        if !skipped.is_empty() {
            out.push_str(&format!("  Skipped:         {} scenarios\n", skipped.len()));
            for s in &skipped {
                if let ScenarioOutcome::Skipped { reason } = &s.outcome {
                    out.push_str(&format!(
                        "                   - {}: {}\n",
                        s.scenario, reason
                    ));
                }
            }
        }
        out.push('\n');

        // Per-axis.
        if !self.per_axis.is_empty() {
            out.push_str("  Per axis:\n");
            for ax in &self.per_axis {
                out.push_str(&format!(
                    "    {:14} {} net win rate (won {} / lost {} / tied {})\n",
                    format!("{}:", ax.axis),
                    format_signed_percent(ax.totals.net_win_rate()),
                    ax.totals.current_wins,
                    ax.totals.baseline_wins,
                    ax.totals.ties,
                ));
            }
            out.push('\n');
        }

        if !verbose {
            out.push_str("  See --verbose for per-scenario breakdown.\n");
            return out;
        }

        // Per-scenario (verbose only).
        out.push_str("  Per scenario:\n");
        for sr in &self.scenarios {
            match &sr.outcome {
                ScenarioOutcome::Graded {
                    per_axis,
                    per_run_scores,
                } => {
                    out.push_str(&format!(
                        "    {} ({} runs):\n",
                        sr.scenario,
                        per_run_scores.len()
                    ));
                    for ax in per_axis {
                        out.push_str(&format!(
                            "      {:14} {} (won {} / lost {} / tied {})\n",
                            format!("{}:", ax.axis),
                            format_signed_percent(ax.totals.net_win_rate()),
                            ax.totals.current_wins,
                            ax.totals.baseline_wins,
                            ax.totals.ties,
                        ));
                    }
                }
                ScenarioOutcome::Skipped { reason } => {
                    out.push_str(&format!("    {} — SKIPPED: {}\n", sr.scenario, reason));
                }
            }
        }

        out
    }
}

/// Format a net win rate as a signed percentage with one decimal.
/// `0.022` → `"+2.2%"`; `-0.014` → `"-1.4%"`; `0.0` → `"+0.0%"`.
fn format_signed_percent(v: f64) -> String {
    if v >= 0.0 {
        format!("+{:.1}%", v * 100.0)
    } else {
        format!("{:.1}%", v * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_win_rate_zero_on_empty() {
        let t = ReportTotals::default();
        assert_eq!(t.net_win_rate(), 0.0);
    }

    #[test]
    fn net_win_rate_pos_when_more_wins() {
        let t = ReportTotals {
            current_wins: 7,
            baseline_wins: 3,
            ties: 0,
        };
        // (7-3) / 10 = 0.4
        assert!((t.net_win_rate() - 0.4).abs() < 1e-12);
    }

    #[test]
    fn net_win_rate_neg_when_more_losses() {
        let t = ReportTotals {
            current_wins: 2,
            baseline_wins: 5,
            ties: 3,
        };
        // (2-5) / 10 = -0.3
        assert!((t.net_win_rate() + 0.3).abs() < 1e-12);
    }

    #[test]
    fn ties_dilute_net_win_rate_but_dont_change_sign() {
        let t_with_ties = ReportTotals {
            current_wins: 1,
            baseline_wins: 0,
            ties: 9,
        };
        let t_no_ties = ReportTotals {
            current_wins: 1,
            baseline_wins: 0,
            ties: 0,
        };
        assert!(t_with_ties.net_win_rate() < t_no_ties.net_win_rate());
        assert!(t_with_ties.net_win_rate() > 0.0);
    }

    #[test]
    fn non_regression_rate_full_when_no_losses() {
        let t = ReportTotals {
            current_wins: 5,
            baseline_wins: 0,
            ties: 5,
        };
        assert!((t.non_regression_rate() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn non_regression_rate_one_on_empty() {
        // Vacuous truth: no verdicts means no regressions observed.
        let t = ReportTotals::default();
        assert!((t.non_regression_rate() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn add_folds_verdicts_correctly() {
        let mut t = ReportTotals::default();
        t.add(Verdict::CurrentWins);
        t.add(Verdict::CurrentWins);
        t.add(Verdict::BaselineWins);
        t.add(Verdict::Tie);
        assert_eq!(
            t,
            ReportTotals {
                current_wins: 2,
                baseline_wins: 1,
                ties: 1,
            }
        );
    }

    #[test]
    fn report_headline_sums_per_axis() {
        // Aggregation contract: headline_totals = sum of per_axis totals.
        let per_axis = [
            AxisTotals {
                axis: Axis::Correctness,
                totals: ReportTotals {
                    current_wins: 2,
                    baseline_wins: 1,
                    ties: 0,
                },
            },
            AxisTotals {
                axis: Axis::Efficiency,
                totals: ReportTotals {
                    current_wins: 1,
                    baseline_wins: 1,
                    ties: 1,
                },
            },
            AxisTotals {
                axis: Axis::Conciseness,
                totals: ReportTotals {
                    current_wins: 0,
                    baseline_wins: 0,
                    ties: 3,
                },
            },
        ];
        let headline = ReportTotals {
            current_wins: 3,
            baseline_wins: 2,
            ties: 4,
        };
        let summed: ReportTotals = per_axis.iter().fold(ReportTotals::default(), |mut acc, a| {
            acc.current_wins += a.totals.current_wins;
            acc.baseline_wins += a.totals.baseline_wins;
            acc.ties += a.totals.ties;
            acc
        });
        assert_eq!(headline, summed);
    }

    fn paired(axis: Axis, verdict: Verdict) -> PairedScore {
        PairedScore {
            axis,
            rationale: "ok".into(),
            verdict,
        }
    }

    #[test]
    fn scenario_outcome_skipped_round_trips_via_json() {
        let s = ScenarioOutcome::Skipped {
            reason: "no baseline".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ScenarioOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn scenario_outcome_graded_round_trips_via_json() {
        let g = ScenarioOutcome::Graded {
            per_run_scores: vec![vec![paired(Axis::Correctness, Verdict::CurrentWins)]],
            per_axis: vec![AxisTotals {
                axis: Axis::Correctness,
                totals: ReportTotals {
                    current_wins: 1,
                    baseline_wins: 0,
                    ties: 0,
                },
            }],
        };
        let json = serde_json::to_string(&g).unwrap();
        let back: ScenarioOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }

    // ── Orchestrator: Report::build_from_results ──

    use crate::eval::{
        baseline::BaselineFile,
        judge::{ComparePair, JudgeAdapter},
        results::{ResultsFile, ScenarioResults},
        score::CompareVerdict,
        transcript::NormalizedTranscript,
    };
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn empty_transcript() -> NormalizedTranscript {
        NormalizedTranscript {
            events: vec![],
            deterministic_floor_passed: true,
            usage_summary: Default::default(),
        }
    }

    fn results_file_with(scenarios: Vec<(&str, usize)>) -> ResultsFile {
        let mut map = std::collections::BTreeMap::new();
        for (name, runs) in scenarios {
            map.insert(
                name.to_string(),
                ScenarioResults {
                    user_turns: vec!["go".into()],
                    runs: (0..runs).map(|_| empty_transcript()).collect(),
                },
            );
        }
        ResultsFile {
            git_ref: "abcdef".into(),
            recorded_at: "2026-05-12T00:00:00Z".into(),
            model: "test/model".into(),
            scenarios: map,
        }
    }

    fn write_baseline(dir: &std::path::Path, scenario: &str, model: &str) {
        let bf = BaselineFile {
            scenario: scenario.into(),
            model: model.into(),
            git_ref: "baseline-ref".into(),
            frozen_at: "2026-05-01T00:00:00Z".into(),
            user_turns: vec!["go".into()],
            transcript: empty_transcript(),
        };
        let path = crate::eval::baseline::baseline_path(dir, scenario, model).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        bf.write_to_path(&path).unwrap();
    }

    /// Canned judge for testing `Report::build_from_results`.
    /// Tracks call count to support fail-then-succeed retry tests.
    struct FakeJudge {
        /// On each call: if the call index is < `fail_until`, return
        /// Err; otherwise return Ok with all-CurrentWins.
        fail_until: usize,
        call_count: Mutex<usize>,
    }

    impl FakeJudge {
        fn all_wins() -> Self {
            Self {
                fail_until: 0,
                call_count: Mutex::new(0),
            }
        }
        fn fail_n_then_wins(n: usize) -> Self {
            Self {
                fail_until: n,
                call_count: Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl JudgeAdapter for FakeJudge {
        async fn compare(
            &self,
            _pair: ComparePair<'_>,
            axes: &[Axis],
            _user_turns: &[String],
            _scenario_judge_model: Option<&str>,
        ) -> anyhow::Result<CompareVerdict> {
            let n = {
                let mut c = self.call_count.lock().unwrap();
                let prev = *c;
                *c += 1;
                prev
            };
            if n < self.fail_until {
                anyhow::bail!("simulated transient error (call #{n})");
            }
            Ok(axes
                .iter()
                .map(|a| PairedScore {
                    axis: *a,
                    rationale: "fake".into(),
                    verdict: Verdict::CurrentWins,
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn build_from_results_grades_when_baselines_present() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 2)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        // 1 scenario × 2 runs × 3 axes (default) = 6 CurrentWins.
        assert_eq!(report.headline_totals.current_wins, 6);
        assert_eq!(report.headline_totals.baseline_wins, 0);
        assert_eq!(report.headline_totals.ties, 0);
        assert_eq!(report.scenarios.len(), 1);
        assert!(matches!(
            report.scenarios[0].outcome,
            ScenarioOutcome::Graded { .. }
        ));
    }

    #[tokio::test]
    async fn build_from_results_skips_when_baseline_missing() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("missing-scenario", 1)]);
        // No baseline written.

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.headline_totals.total(), 0);
        assert_eq!(report.scenarios.len(), 1);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("no baseline"),
                    "expected 'no baseline' diagnostic; got: {reason}"
                );
                // Spec: error suggestion must be copy-pasteable.
                assert!(
                    reason.contains("steve eval baseline freeze"),
                    "expected exact freeze command in diagnostic; got: {reason}"
                );
            }
            other => panic!("expected Skipped; got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_from_results_retries_once_on_transient_judge_error() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        // Fail once, then succeed.
        let judge = FakeJudge::fail_n_then_wins(1);
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        // The retry succeeded, so the single run is graded.
        assert_eq!(report.headline_totals.total(), 3); // 3 axes
    }

    #[tokio::test]
    async fn build_from_results_marks_errored_when_both_attempts_fail() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        // Fail twice (try + retry both fail).
        let judge = FakeJudge::fail_n_then_wins(2);
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        // Single run errored on both attempts. All-runs-errored for
        // the scenario → Skipped per spec.
        assert_eq!(report.headline_totals.total(), 0);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(reason.contains("errored"), "got: {reason}");
            }
            other => panic!("expected Skipped; got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_from_results_per_axis_in_default_order() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        // DEFAULT_AXES order: Correctness, Efficiency, Conciseness.
        let axis_order: Vec<Axis> = report.per_axis.iter().map(|a| a.axis).collect();
        assert_eq!(
            axis_order,
            vec![Axis::Correctness, Axis::Efficiency, Axis::Conciseness]
        );
    }

    #[tokio::test]
    async fn build_from_results_records_baseline_provenance() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            "fake/judge-model",
            None,
        )
        .await
        .unwrap();

        let prov = report.baseline_provenance.get("_smoke").unwrap();
        assert_eq!(prov.git_ref, "baseline-ref");
        assert_eq!(prov.frozen_at, "2026-05-01T00:00:00Z");
    }

    // ── Text rendering ──

    fn empty_report() -> Report {
        Report {
            model: "test/model".into(),
            results_git_ref: "abc1234".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "fake/judge".into(),
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: Vec::new(),
        }
    }

    #[test]
    fn render_text_contains_headline_with_signed_percentage() {
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 4,
            baseline_wins: 2,
            ties: 24,
        };
        r.per_axis = vec![AxisTotals {
            axis: Axis::Correctness,
            totals: ReportTotals {
                current_wins: 1,
                baseline_wins: 2,
                ties: 7,
            },
        }];
        let out = r.render_text(false);
        // Headline: (4-2)/30 = +0.067 → +6.7%; 28/30 = 93.3%
        assert!(out.contains("Headline"), "got:\n{out}");
        assert!(
            out.contains("+6.7%"),
            "expected +6.7% headline; got:\n{out}"
        );
        assert!(out.contains("93.3%"), "got:\n{out}");
    }

    #[test]
    fn render_text_lists_per_axis_section_when_axes_present() {
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 2,
            baseline_wins: 1,
            ties: 0,
        };
        r.per_axis = vec![
            AxisTotals {
                axis: Axis::Correctness,
                totals: ReportTotals {
                    current_wins: 1,
                    baseline_wins: 1,
                    ties: 0,
                },
            },
            AxisTotals {
                axis: Axis::Efficiency,
                totals: ReportTotals {
                    current_wins: 1,
                    baseline_wins: 0,
                    ties: 0,
                },
            },
        ];
        let out = r.render_text(false);
        assert!(out.contains("Per axis"), "got:\n{out}");
        assert!(out.contains("correctness"), "got:\n{out}");
        assert!(out.contains("efficiency"), "got:\n{out}");
    }

    #[test]
    fn render_text_omits_per_scenario_when_not_verbose() {
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 1,
            baseline_wins: 0,
            ties: 0,
        };
        r.scenarios = vec![ScenarioReport {
            scenario: "_smoke".into(),
            outcome: ScenarioOutcome::Graded {
                per_run_scores: vec![vec![]],
                per_axis: Vec::new(),
            },
        }];
        let out = r.render_text(false);
        assert!(
            !out.contains("_smoke"),
            "scenario name should not appear in non-verbose; got:\n{out}"
        );
        assert!(
            out.contains("See --verbose"),
            "expected hint to use --verbose; got:\n{out}"
        );
    }

    #[test]
    fn render_text_includes_per_scenario_when_verbose() {
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 1,
            baseline_wins: 0,
            ties: 0,
        };
        r.scenarios = vec![ScenarioReport {
            scenario: "_smoke".into(),
            outcome: ScenarioOutcome::Graded {
                per_run_scores: vec![vec![]],
                per_axis: vec![AxisTotals {
                    axis: Axis::Correctness,
                    totals: ReportTotals {
                        current_wins: 1,
                        baseline_wins: 0,
                        ties: 0,
                    },
                }],
            },
        }];
        let out = r.render_text(true);
        assert!(
            out.contains("_smoke"),
            "verbose must include scenario name; got:\n{out}"
        );
    }

    #[test]
    fn render_text_lists_skipped_scenarios_with_reason() {
        let mut r = empty_report();
        r.scenarios = vec![ScenarioReport {
            scenario: "missing-bl".into(),
            outcome: ScenarioOutcome::Skipped {
                reason: "no baseline for X".into(),
            },
        }];
        let out = r.render_text(false);
        assert!(out.contains("Skipped"), "got:\n{out}");
        assert!(out.contains("missing-bl"), "got:\n{out}");
        assert!(out.contains("no baseline"), "got:\n{out}");
    }

    #[test]
    fn render_text_shows_baseline_provenance_when_single_ref() {
        let mut r = empty_report();
        r.baseline_provenance.insert(
            "_smoke".into(),
            BaselineProvenance {
                git_ref: "abc1234".into(),
                frozen_at: "2026-05-01T00:00:00Z".into(),
            },
        );
        r.baseline_provenance.insert(
            "other".into(),
            BaselineProvenance {
                git_ref: "abc1234".into(),
                frozen_at: "2026-05-01T00:00:00Z".into(),
            },
        );
        let out = r.render_text(false);
        assert!(out.contains("baseline frozen"), "got:\n{out}");
        assert!(out.contains("abc1234"), "got:\n{out}");
    }
}
