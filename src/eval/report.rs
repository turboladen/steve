//! Aggregation of paired-comparison verdicts into the layered
//! report — headline + per-axis + per-scenario detail.
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
        /// One `Vec<PairedScore>` per run that graded successfully.
        /// Length may be less than the run count in the results file
        /// if some runs errored on both judge-call attempts.
        per_run_scores: Vec<Vec<PairedScore>>,
        /// Per-axis tally for this scenario alone, for the verbose
        /// per-scenario rendering.
        per_axis: Vec<AxisTotals>,
        /// Count of runs that errored on both judge-call attempts
        /// and were excluded from `per_run_scores`. Surfaced in
        /// `--verbose` output so operators see that the sample
        /// size shrank below the configured K. `errored_runs.0`
        /// is the count; `errored_runs.1` is the last error
        /// message for context.
        #[serde(default)]
        errored_runs: ErroredRuns,
    },
    Skipped {
        /// Human-readable reason — the renderer prints this in the
        /// "Skipped:" subsection of the headline. Typical values:
        /// `"no baseline for scenario X with model Y: run …"` or
        /// `"all K runs of scenario X errored: <last error msg>"`.
        reason: String,
    },
}

/// Count of runs that errored on both judge-call attempts +
/// the last error message. Surfaced in `--verbose` rendering so
/// operators see when transient judge flakiness has shrunk the
/// sample size below the configured `scenario.runs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErroredRuns {
    pub count: usize,
    pub last_error: Option<String>,
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

/// Tally of how many `NormalizedTranscript`s passed the
/// deterministic rule-based assertion floor at eval-run time.
/// Sourced from each transcript's `deterministic_floor_passed`
/// bool; surfaced in history rows so cumulative-floor regressions
/// are visible in the trend chart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicFloor {
    pub passed: usize,
    pub total: usize,
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
    /// Judge model used for this report. `Some("provider/model")` when
    /// a single judge model is in effect (CLI flag or eval config
    /// default). `None` when each scenario uses its own
    /// `scenario.judge_model` — the renderer surfaces this as
    /// "per-scenario" prose rather than a placeholder string that
    /// would poison downstream tooling that parses the field as
    /// `provider/model_id`.
    pub judge_model: Option<String>,
    /// Suite-wide tally across all (scenario × run × axis) cells
    /// that were graded.
    pub headline_totals: ReportTotals,
    /// Per-axis slice of `headline_totals`. Order is spec-axis order
    /// (typically Correctness, Efficiency, Conciseness, but may differ
    /// when scenarios override `[scoring].axes`).
    pub per_axis: Vec<AxisTotals>,
    /// Per-scenario detail. Order matches results-file insertion order.
    pub scenarios: Vec<ScenarioReport>,
    /// Aggregate deterministic-floor tally across every run in the
    /// results file. Computed from each transcript's
    /// `deterministic_floor_passed` bool.
    #[serde(default)]
    pub deterministic_floor: DeterministicFloor,
}

impl Report {
    /// Build a `Report` by walking every (scenario, run) pair in
    /// `results`, resolving a baseline from `baselines_dir`, and
    /// calling `judge.compare(...)` for each cell. Missing baselines
    /// surface as `Skipped`. Transient judge errors are retried once;
    /// double-failures exclude the run from `per_run_scores`.
    /// All-runs-errored on a scenario maps to `Skipped` per spec.
    ///
    /// `scenarios_dir` is `Option` because production callers always
    /// pass `Some(eval/scenarios)` to read per-scenario `[scoring].axes`
    /// and `judge_model` overrides, but unit tests pass `None` to keep
    /// the fake judge focused on orchestration logic (axes default to
    /// `DEFAULT_AXES`, scenario_judge_model defaults to `None`).
    pub async fn build_from_results(
        results: &crate::eval::results::ResultsFile,
        baselines_dir: &std::path::Path,
        results_path: &str,
        judge: &dyn crate::eval::JudgeAdapter,
        judge_model: Option<&str>,
        scenarios_dir: Option<&std::path::Path>,
        config_default_judge_model: Option<&str>,
    ) -> anyhow::Result<Self> {
        use crate::eval::{
            baseline::{BaselineFile, baseline_path},
            scenario::Scenario,
        };

        // Compute aggregate deterministic-floor tally from every
        // transcript in the results file. Counts ALL runs, not just
        // graded ones (Skipped scenarios still ran the rule-based
        // assertions; we surface their floor pass/fail too).
        let mut floor = DeterministicFloor::default();
        for scn in results.scenarios.values() {
            for t in &scn.runs {
                floor.total += 1;
                if t.deterministic_floor_passed {
                    floor.passed += 1;
                }
            }
        }

        let mut report = Report {
            model: results.model.clone(),
            results_git_ref: results.git_ref.clone(),
            results_path: results_path.to_string(),
            baseline_provenance: BTreeMap::new(),
            judge_model: judge_model.map(str::to_string),
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: Vec::new(),
            deterministic_floor: floor,
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
            // Determine axes + per-scenario judge model from the
            // on-disk scenario.toml when `scenarios_dir` was supplied.
            // A parse failure (typo, schema drift) MUST not silently
            // fall back to DEFAULT_AXES — the operator would see a
            // clean "Graded" report grading against the wrong axes.
            // NotFound is the only legitimate fallback: it just means
            // the report was invoked without a scenarios root.
            let scenario_on_disk = match scenarios_dir {
                Some(dir) => {
                    let path = dir.join(scenario_name).join("scenario.toml");
                    match Scenario::from_file(&path) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            // When `scenarios_dir` was supplied the
                            // operator expects every scenario in the
                            // results file to have a matching
                            // `scenario.toml` under it. Either a parse
                            // failure or a missing manifest deserves
                            // Skipped with a clear diagnostic —
                            // silently falling back to DEFAULT_AXES
                            // would re-introduce the silent
                            // grading-against-wrong-axes failure mode.
                            let is_not_found = e
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|ioe| ioe.kind() == std::io::ErrorKind::NotFound);
                            let reason = if is_not_found {
                                format!(
                                    "scenario.toml not found at {}; \
                                     results file references a scenario that no \
                                     longer exists under the scenarios root",
                                    path.display()
                                )
                            } else {
                                format!("scenario.toml load failed at {}: {e:#}", path.display())
                            };
                            report.scenarios.push(ScenarioReport {
                                scenario: scenario_name.clone(),
                                outcome: ScenarioOutcome::Skipped { reason },
                            });
                            continue;
                        }
                    }
                }
                None => None,
            };
            let axes: Vec<Axis> = match &scenario_on_disk {
                Some(scn) => scn.scoring_axes().to_vec(),
                None => crate::eval::score::DEFAULT_AXES.to_vec(),
            };
            let scenario_judge_model: Option<String> = scenario_on_disk
                .as_ref()
                .and_then(|scn| scn.judge_model.clone());

            // user_turns drift: the baseline and the current results
            // MUST share the same user prompts, otherwise the judge
            // is comparing transcripts that responded to different
            // questions. Skip with a re-freeze hint instead of
            // producing misleading verdicts.
            if baseline.user_turns != scenario_results.user_turns {
                // Branch on count vs content so a same-count-different-text
                // drift doesn't render as the confusing "{n} turn(s) vs
                // {n} turn(s)" — the lengths match in that case and the
                // operator's first instinct would be "this can't be the
                // right diagnostic." Point at the first differing turn
                // instead so the actual drift is locatable.
                let bl = baseline.user_turns.len();
                let cur = scenario_results.user_turns.len();
                let detail = if bl == cur {
                    let first_diff = baseline
                        .user_turns
                        .iter()
                        .zip(&scenario_results.user_turns)
                        .position(|(a, b)| a != b)
                        .map(|i| i + 1)
                        .unwrap_or(1);
                    format!(
                        "{bl} turn(s) on both sides but content differs (first at turn {first_diff})"
                    )
                } else {
                    format!("baseline was frozen against {bl} turn(s), results have {cur} turn(s)")
                };
                report.scenarios.push(ScenarioReport {
                    scenario: scenario_name.clone(),
                    outcome: ScenarioOutcome::Skipped {
                        reason: format!(
                            "scenario '{scenario_name}' user_turns drifted from baseline: \
                             {detail}; \
                             re-run `steve eval baseline freeze --scenario {scenario_name} --model {}`",
                            results.model
                        ),
                    },
                });
                continue;
            }

            // Special-case empty runs Vec — conflating "zero runs in
            // the results file" with "all K runs errored" would be a
            // misleading diagnostic. Tell the operator to re-run.
            if scenario_results.runs.is_empty() {
                report.scenarios.push(ScenarioReport {
                    scenario: scenario_name.clone(),
                    outcome: ScenarioOutcome::Skipped {
                        reason: format!(
                            "results file contains zero runs for scenario '{scenario_name}'; \
                             re-run `steve eval run --scenario {scenario_name} --model {}`",
                            results.model
                        ),
                    },
                });
                continue;
            }

            // Precedence at the judge-call site: scenario_judge_model
            // wins over config_default_judge_model. CLI override is
            // already inside `judge` (highest precedence).
            // Collapsing config_default into the judge's CLI slot
            // upstream would make config_default beat per-scenario
            // overrides, which contradicts the documented precedence.
            let effective_scenario_judge: Option<String> = scenario_judge_model
                .clone()
                .or_else(|| config_default_judge_model.map(str::to_string));

            // Walk each run; call judge with retry-once.
            let mut per_run_scores: Vec<Vec<PairedScore>> =
                Vec::with_capacity(scenario_results.runs.len());
            let mut errored_count: usize = 0;
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
                            effective_scenario_judge.as_deref(),
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
                match scores {
                    Some(s) => per_run_scores.push(s),
                    None => errored_count += 1,
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
            // Record provenance only for scenarios that actually
            // graded — Skipped scenarios (parse failure, user_turns
            // drift, missing scenario.toml, all-runs-errored) MUST NOT
            // contribute to the metadata header's scenario count,
            // which would otherwise overstate how many scenarios were
            // anchored against the baseline.
            report.baseline_provenance.insert(
                scenario_name.clone(),
                BaselineProvenance {
                    git_ref: baseline.git_ref.clone(),
                    frozen_at: baseline.frozen_at.clone(),
                },
            );
            report.scenarios.push(ScenarioReport {
                scenario: scenario_name.clone(),
                outcome: ScenarioOutcome::Graded {
                    per_run_scores,
                    per_axis: scenario_per_axis_vec,
                    errored_runs: ErroredRuns {
                        count: errored_count,
                        last_error,
                    },
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
            // No scenarios contributed verdicts (every one was Skipped:
            // missing baseline, user_turns drift, all-runs-errored, etc.).
            // Without this line, the operator sees a blank line between
            // the header and a meaningless `0.000 net win rate` headline
            // — no signal that the emptiness is *why* the headline looks
            // the way it does. Per-scenario reasons are in the Scenarios
            // section below; the stderr "no scenarios graded" message
            // covers the `steve eval | tee log.txt` case where stdout is
            // captured but stderr isn't.
            out.push_str(
                "  no scenarios graded against any baseline \
                 (see Scenarios section below for per-scenario reasons)\n\n",
            );
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
            // Per-scenario baseline provenance — delivers on the
            // "see --verbose" hint emitted from the metadata block
            // when scenarios pin to varying baseline git_refs.
            // Skipped scenarios have no provenance entry (no
            // baseline to anchor against), so this only fires for
            // Graded ones.
            let provenance_line = self
                .baseline_provenance
                .get(&sr.scenario)
                .map(|p| format!("      baseline: frozen {} at {}\n", p.frozen_at, p.git_ref))
                .unwrap_or_default();
            match &sr.outcome {
                ScenarioOutcome::Graded {
                    per_axis,
                    per_run_scores,
                    errored_runs,
                } => {
                    if errored_runs.count > 0 {
                        out.push_str(&format!(
                            "    {} ({} runs graded, {} errored):\n",
                            sr.scenario,
                            per_run_scores.len(),
                            errored_runs.count,
                        ));
                        if let Some(err) = &errored_runs.last_error {
                            out.push_str(&format!("      last error: {err}\n"));
                        }
                    } else {
                        out.push_str(&format!(
                            "    {} ({} runs):\n",
                            sr.scenario,
                            per_run_scores.len()
                        ));
                    }
                    out.push_str(&provenance_line);
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

    #[tokio::test]
    async fn build_from_results_headline_equals_sum_of_per_axis() {
        // Aggregation contract: report.headline_totals must equal
        // sum of report.per_axis totals. Pinned end-to-end through
        // build_from_results rather than in isolation — a previous
        // tautological version of this test summed values the test
        // itself constructed, exercising no production code.
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 3)]); // 3 runs × 3 axes
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge"),
            None,
            None,
        )
        .await
        .unwrap();

        // Sum per-axis tallies and assert against headline.
        let summed = report
            .per_axis
            .iter()
            .fold(ReportTotals::default(), |mut acc, a| {
                acc.current_wins += a.totals.current_wins;
                acc.baseline_wins += a.totals.baseline_wins;
                acc.ties += a.totals.ties;
                acc
            });
        assert_eq!(report.headline_totals, summed);
        // Sanity: 3 runs × 3 axes = 9 cells, all CurrentWins.
        assert_eq!(report.headline_totals.current_wins, 9);
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
            errored_runs: Default::default(),
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
        /// Err; otherwise return Ok with `verdict` for every axis.
        fail_until: usize,
        call_count: Mutex<usize>,
        /// Records every `scenario_judge_model` value passed in —
        /// used by precedence tests to assert which judge model
        /// reached the compare site.
        scenario_judge_calls: Mutex<Vec<Option<String>>>,
        verdict: Verdict,
    }

    impl FakeJudge {
        fn all_wins() -> Self {
            Self {
                fail_until: 0,
                call_count: Mutex::new(0),
                scenario_judge_calls: Mutex::new(Vec::new()),
                verdict: Verdict::CurrentWins,
            }
        }
        fn fail_n_then_wins(n: usize) -> Self {
            Self {
                fail_until: n,
                call_count: Mutex::new(0),
                scenario_judge_calls: Mutex::new(Vec::new()),
                verdict: Verdict::CurrentWins,
            }
        }
        fn all_with(verdict: Verdict) -> Self {
            Self {
                fail_until: 0,
                call_count: Mutex::new(0),
                scenario_judge_calls: Mutex::new(Vec::new()),
                verdict,
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
            scenario_judge_model: Option<&str>,
        ) -> anyhow::Result<CompareVerdict> {
            self.scenario_judge_calls
                .lock()
                .unwrap()
                .push(scenario_judge_model.map(str::to_string));
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
                    verdict: self.verdict,
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
            Some("fake/judge-model"),
            None,
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
            Some("fake/judge-model"),
            None,
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
                assert!(
                    reason.contains("steve eval baseline freeze"),
                    "expected exact freeze command in diagnostic; got: {reason}"
                );
            }
            other => panic!("expected Skipped; got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_from_results_skips_scenario_when_scenario_toml_is_malformed() {
        // A scenario.toml load failure MUST surface as Skipped (with
        // the failure in the reason) rather than silently falling
        // back to DEFAULT_AXES — silent fallback produces a clean
        // Graded report grading against the wrong axes when the
        // operator has a typo. A regression replacing the match with
        // `.ok()` would be caught here.
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        let scenarios = tmp.path().join("scenarios");
        std::fs::create_dir_all(scenarios.join("_smoke")).unwrap();
        std::fs::write(
            scenarios.join("_smoke/scenario.toml"),
            "this is { not valid toml",
        )
        .unwrap();

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge-model"),
            Some(&scenarios),
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.scenarios.len(), 1);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("scenario.toml load failed"),
                    "reason must surface load failure; got: {reason}"
                );
                assert!(
                    reason.contains("_smoke"),
                    "reason must identify the offending scenario; got: {reason}"
                );
            }
            other => panic!("expected Skipped on malformed scenario.toml; got {other:?}"),
        }
        // Malformed scenario must NOT contribute any verdicts OR
        // provenance — provenance is reserved for Graded scenarios
        // so the metadata header's "(N scenarios)" count stays
        // honest.
        assert_eq!(report.headline_totals.current_wins, 0);
        assert_eq!(report.headline_totals.baseline_wins, 0);
        assert_eq!(report.headline_totals.ties, 0);
        assert!(report.baseline_provenance.is_empty());
    }

    #[tokio::test]
    async fn build_from_results_skips_when_scenario_toml_missing_under_scenarios_dir() {
        // Operator-passed `scenarios_dir` means "every scenario in
        // results MUST have a manifest here". A missing scenario.toml
        // (NotFound) is silent grading-against-DEFAULT_AXES otherwise.
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");
        // scenarios root exists but the _smoke subdir is absent.
        let scenarios = tmp.path().join("scenarios");
        std::fs::create_dir_all(&scenarios).unwrap();

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge-model"),
            Some(&scenarios),
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.scenarios.len(), 1);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("not found"),
                    "expected NotFound diagnostic; got: {reason}"
                );
                assert!(
                    reason.contains("_smoke"),
                    "expected scenario name in diagnostic; got: {reason}"
                );
            }
            other => panic!("expected Skipped on missing scenario.toml; got {other:?}"),
        }
        assert_eq!(report.headline_totals.total(), 0);
    }

    /// Helper for the judge_model precedence tests: writes a valid
    /// scenario.toml under `scenarios_dir/_smoke/` with the supplied
    /// `judge_model` and a minimal user_turn matching the test
    /// fixtures' baseline + results.
    fn write_scenario_with_judge_model(scenarios_dir: &std::path::Path, judge: Option<&str>) {
        let dir = scenarios_dir.join("_smoke");
        std::fs::create_dir_all(&dir).unwrap();
        let judge_line = match judge {
            Some(j) => format!("judge_model = \"{j}\"\n"),
            None => String::new(),
        };
        let toml = format!(
            r#"name = "_smoke"
description = "fixture"
user_turns = ["go"]
{judge_line}
[[expectations]]
kind = "final_message_contains"
substring = "ok"
"#
        );
        std::fs::write(dir.join("scenario.toml"), toml).unwrap();
    }

    #[tokio::test]
    async fn build_from_results_uses_scenario_judge_over_config_default() {
        // Precedence MUST be: CLI > scenario.judge_model > config_default.
        // Collapsing config_default into Judge.cli_model upstream would
        // make it beat per-scenario overrides — this pins the contract
        // that scenario.toml's judge wins over any config-default the
        // orchestrator threads in.
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");
        let scenarios = tmp.path().join("scenarios");
        write_scenario_with_judge_model(&scenarios, Some("scenario/judge"));

        let judge = FakeJudge::all_wins();
        let _report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            None, // no CLI override
            Some(&scenarios),
            Some("config/default"), // config_default present
        )
        .await
        .unwrap();

        let calls = judge.scenario_judge_calls.lock().unwrap().clone();
        assert!(!calls.is_empty(), "judge should have been called");
        for (i, call) in calls.iter().enumerate() {
            assert_eq!(
                call.as_deref(),
                Some("scenario/judge"),
                "call #{i}: scenario.judge_model MUST win over config_default; got {call:?}",
            );
        }
    }

    #[tokio::test]
    async fn build_from_results_uses_config_default_when_scenario_has_no_judge_model() {
        // When scenario.toml has no judge_model, config_default
        // fills in. CLI not set.
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 1)]);
        write_baseline(tmp.path(), "_smoke", "test/model");
        let scenarios = tmp.path().join("scenarios");
        write_scenario_with_judge_model(&scenarios, None); // no judge_model

        let judge = FakeJudge::all_wins();
        let _report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            None,
            Some(&scenarios),
            Some("config/default"),
        )
        .await
        .unwrap();

        let calls = judge.scenario_judge_calls.lock().unwrap().clone();
        assert!(!calls.is_empty());
        for call in &calls {
            assert_eq!(
                call.as_deref(),
                Some("config/default"),
                "config_default MUST fill in when scenario has no judge_model",
            );
        }
    }

    #[tokio::test]
    async fn build_from_results_skips_when_user_turns_drift_from_baseline() {
        // If the scenario's user prompts changed after the baseline
        // was frozen, the judge would be comparing transcripts that
        // responded to different questions — silently misleading.
        // Skip with a "re-freeze" hint instead.
        let tmp = TempDir::new().unwrap();
        let mut results = results_file_with(vec![("_smoke", 1)]);
        // Mutate the current run's user_turns to differ from the
        // baseline's (which write_baseline pins at ["go"]).
        results.scenarios.get_mut("_smoke").unwrap().user_turns =
            vec!["go".into(), "and another step".into()];
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge-model"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.scenarios.len(), 1);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("user_turns drifted"),
                    "expected drift diagnostic; got: {reason}"
                );
                assert!(
                    reason.contains("steve eval baseline freeze"),
                    "expected freeze hint; got: {reason}"
                );
                assert!(
                    reason.contains("1 turn(s)") && reason.contains("2 turn(s)"),
                    "expected baseline/current turn counts; got: {reason}"
                );
            }
            other => panic!("expected Skipped on user_turns drift; got {other:?}"),
        }
        // Drifted scenario must NOT contribute any verdicts AND
        // MUST NOT register a baseline_provenance entry — otherwise
        // the metadata header's "(N scenarios)" count would
        // overstate how many scenarios were actually graded against
        // the baseline.
        assert_eq!(report.headline_totals.total(), 0);
        assert!(
            report.baseline_provenance.is_empty(),
            "Skipped-on-drift scenarios must not contribute provenance; got: {:?}",
            report.baseline_provenance
        );
    }

    #[tokio::test]
    async fn build_from_results_skips_when_user_turns_text_drifts_but_count_matches() {
        // The same-count-different-text case: the baseline's user_turns
        // and the current run's user_turns have the same length but
        // different content (e.g. operator tweaked a turn's wording).
        // The skip-reason must NOT render as "1 turn(s) vs 1 turn(s)" —
        // that's confusing because both numbers match. Instead it must
        // surface the content drift with a pointer at the first
        // differing turn.
        let tmp = TempDir::new().unwrap();
        let mut results = results_file_with(vec![("_smoke", 1)]);
        // Same length as the baseline (write_baseline pins ["go"]), but
        // different text.
        results.scenarios.get_mut("_smoke").unwrap().user_turns = vec!["different prompt".into()];
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge-model"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.scenarios.len(), 1);
        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("user_turns drifted"),
                    "expected drift diagnostic; got: {reason}"
                );
                assert!(
                    reason.contains("content differs"),
                    "expected content-drift wording (NOT a turn-count comparison); got: {reason}"
                );
                assert!(
                    reason.contains("first at turn 1"),
                    "expected pointer at first differing turn; got: {reason}"
                );
                assert!(
                    reason.contains("steve eval baseline freeze"),
                    "expected freeze hint; got: {reason}"
                );
                // The misleading "{n} turn(s) vs {n} turn(s)" wording
                // from the count-only branch MUST NOT appear here.
                assert!(
                    !reason.contains("baseline was frozen against"),
                    "count-vs-count wording leaked into content-drift case: {reason}"
                );
            }
            other => panic!("expected Skipped on user_turns drift; got {other:?}"),
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
            Some("fake/judge-model"),
            None,
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
            Some("fake/judge-model"),
            None,
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
            Some("fake/judge-model"),
            None,
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
    async fn build_from_results_buckets_baseline_wins_correctly() {
        // The all-CurrentWins tests can't catch a copy-paste swap of
        // `current_wins` / `baseline_wins` in the bucket-walk loop.
        // Pin each verdict variant into its own bucket end-to-end.
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 2)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_with(Verdict::BaselineWins);
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge-model"),
            None,
            None,
        )
        .await
        .unwrap();

        // 1 scenario × 2 runs × 3 axes = 6 BaselineWins → all should
        // land in baseline_wins, never current_wins.
        assert_eq!(report.headline_totals.current_wins, 0);
        assert_eq!(report.headline_totals.baseline_wins, 6);
        assert_eq!(report.headline_totals.ties, 0);
        for ax in &report.per_axis {
            assert_eq!(
                ax.totals.current_wins, 0,
                "axis {:?} leaked into current",
                ax.axis
            );
            assert_eq!(ax.totals.baseline_wins, 2, "axis {:?} miscount", ax.axis);
        }
    }

    #[tokio::test]
    async fn build_from_results_buckets_ties_correctly() {
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 2)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_with(Verdict::Tie);
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge-model"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.headline_totals.current_wins, 0);
        assert_eq!(report.headline_totals.baseline_wins, 0);
        assert_eq!(report.headline_totals.ties, 6);
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
            Some("fake/judge-model"),
            None,
            None,
        )
        .await
        .unwrap();

        let prov = report.baseline_provenance.get("_smoke").unwrap();
        assert_eq!(prov.git_ref, "baseline-ref");
        assert_eq!(prov.frozen_at, "2026-05-01T00:00:00Z");
    }

    #[tokio::test]
    async fn build_from_results_aggregates_deterministic_floor() {
        // Pin: each run's `deterministic_floor_passed` bool flows
        // from the transcript through Report.deterministic_floor.
        // A break in this plumbing would render every row as 0/0 in
        // history.jsonl without otherwise failing any test.
        let tmp = TempDir::new().unwrap();
        let mut results = results_file_with(vec![("a", 2), ("b", 1)]);
        // a: 2 runs, both passed.
        for t in results.scenarios.get_mut("a").unwrap().runs.iter_mut() {
            t.deterministic_floor_passed = true;
        }
        // b: 1 run, failed.
        results.scenarios.get_mut("b").unwrap().runs[0].deterministic_floor_passed = false;
        write_baseline(tmp.path(), "a", "test/model");
        write_baseline(tmp.path(), "b", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge"),
            None,
            None,
        )
        .await
        .unwrap();

        // Total = 3 runs across both scenarios; 2 passed the floor.
        assert_eq!(report.deterministic_floor.total, 3);
        assert_eq!(report.deterministic_floor.passed, 2);
    }

    #[tokio::test]
    async fn build_from_results_special_cases_empty_runs_vec() {
        // Empty `runs: vec![]` must NOT collapse into the "all
        // errored" diagnostic — there is no error to report, just
        // an empty file. Surface a re-run hint instead.
        let tmp = TempDir::new().unwrap();
        let mut results = results_file_with(vec![("_smoke", 0)]);
        // Force the runs Vec to empty (results_file_with(_, 0) does
        // produce length 0 today, but be explicit).
        results.scenarios.get_mut("_smoke").unwrap().runs.clear();
        write_baseline(tmp.path(), "_smoke", "test/model");

        let judge = FakeJudge::all_wins();
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge"),
            None,
            None,
        )
        .await
        .unwrap();

        match &report.scenarios[0].outcome {
            ScenarioOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("zero runs"),
                    "expected 'zero runs' diagnostic, got: {reason}"
                );
                assert!(
                    reason.contains("re-run"),
                    "expected re-run hint, got: {reason}"
                );
            }
            other => panic!("expected Skipped for empty-runs scenario; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn build_from_results_surfaces_per_run_errors_in_graded_variant() {
        // Pin: when some runs error but others succeed, the sample
        // size shrinks silently — ErroredRuns must carry both the
        // count AND the last error so the renderer can surface it.
        let tmp = TempDir::new().unwrap();
        let results = results_file_with(vec![("_smoke", 3)]);
        write_baseline(tmp.path(), "_smoke", "test/model");

        // First call fails (retried, fails again → run errored),
        // then subsequent calls succeed. Total: 2 attempts for run 1
        // (both fail), then 1 attempt each for runs 2 & 3 (both pass).
        let judge = FakeJudge::fail_n_then_wins(2);
        let report = Report::build_from_results(
            &results,
            tmp.path(),
            "results.yaml",
            &judge,
            Some("fake/judge"),
            None,
            None,
        )
        .await
        .unwrap();

        match &report.scenarios[0].outcome {
            ScenarioOutcome::Graded {
                per_run_scores,
                errored_runs,
                ..
            } => {
                // 2 runs graded (run 2 and run 3), 1 errored (run 1).
                assert_eq!(
                    per_run_scores.len(),
                    2,
                    "expected 2 successful runs, got {}",
                    per_run_scores.len()
                );
                assert_eq!(errored_runs.count, 1, "expected 1 errored run");
                assert!(
                    errored_runs.last_error.is_some(),
                    "expected last_error to carry the simulated error message"
                );
            }
            other => panic!("expected Graded with 1 errored, got {other:?}"),
        }
    }

    // ── Text rendering ──

    fn empty_report() -> Report {
        Report {
            model: "test/model".into(),
            results_git_ref: "abc1234".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: Some("fake/judge".into()),
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: Vec::new(),
            deterministic_floor: Default::default(),
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
                errored_runs: Default::default(),
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
                errored_runs: Default::default(),
            },
        }];
        let out = r.render_text(true);
        assert!(
            out.contains("_smoke"),
            "verbose must include scenario name; got:\n{out}"
        );
    }

    #[test]
    fn render_text_surfaces_errored_runs_with_last_error_in_verbose() {
        // Spec mandates errored runs surface in --verbose output
        // (paired-comparison spec, "judge-call failure handling").
        // Pin both the "(X runs graded, Y errored)" status and the
        // "last error: ..." line so refactors can't silently delete
        // the spec-required visibility.
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 3,
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
                errored_runs: ErroredRuns {
                    count: 2,
                    last_error: Some("simulated 503".into()),
                },
            },
        }];
        let out = r.render_text(true);
        assert!(
            out.contains("1 runs graded, 2 errored"),
            "expected the 'X graded, Y errored' status; got:\n{out}"
        );
        assert!(
            out.contains("last error: simulated 503"),
            "expected the last_error line; got:\n{out}"
        );
    }

    #[test]
    fn render_text_omits_errored_runs_line_when_count_is_zero() {
        // Negative case: clean graded outcomes should NOT carry an
        // "errored" suffix on the status line.
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 3,
            baseline_wins: 0,
            ties: 0,
        };
        r.scenarios = vec![ScenarioReport {
            scenario: "_smoke".into(),
            outcome: ScenarioOutcome::Graded {
                per_run_scores: vec![vec![]],
                per_axis: Vec::new(),
                errored_runs: Default::default(),
            },
        }];
        let out = r.render_text(true);
        assert!(
            !out.contains("errored"),
            "clean Graded scenarios must NOT mention 'errored'; got:\n{out}"
        );
        assert!(
            !out.contains("last error:"),
            "clean Graded scenarios must NOT mention 'last error:'; got:\n{out}"
        );
    }

    #[test]
    fn render_text_does_not_surface_judge_model_field_when_none() {
        // The text renderer deliberately does NOT include the
        // judge_model field — that surface is HTML metadata + history
        // JSONL only. Pin "doesn't surface" explicitly so a future PR
        // that adds judge_model to render_text can't accidentally
        // unwrap or Debug-print the Option<String> on the None case.
        let mut r = empty_report();
        r.judge_model = None;
        r.headline_totals = ReportTotals {
            current_wins: 1,
            baseline_wins: 0,
            ties: 0,
        };
        let out = r.render_text(false);
        assert!(
            !out.contains("judge_model"),
            "text renderer must not surface judge_model field; got:\n{out}"
        );
        // Defense against `Debug` formatting on the Option<String>:
        // `Some("x")` and `None` are both Debug forms that should
        // never reach output.
        assert!(
            !out.contains("Some(") && !out.contains("None"),
            "text renderer must not Debug-print Option<String>; got:\n{out}"
        );
        // The previous "<per-scenario>" placeholder string must also
        // never appear (it was removed in favor of Option<String>).
        assert!(
            !out.contains("<per-scenario>"),
            "old placeholder string must not leak into text; got:\n{out}"
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
    fn render_text_surfaces_empty_baseline_provenance_explicitly() {
        // When every scenario is Skipped, no baseline is consulted and
        // `baseline_provenance` is empty. The render must emit a line
        // saying so — without it, the operator sees a blank line and a
        // meaningless `0.000 net win rate` headline with no signal that
        // the emptiness is *why* the headline looks the way it does.
        // The `Scenarios section below` pointer references the per-scenario
        // reasons that follow (`Skipped: <reason>` lines).
        let mut r = empty_report();
        r.scenarios = vec![ScenarioReport {
            scenario: "no-baseline-scenario".into(),
            outcome: ScenarioOutcome::Skipped {
                reason: "no baseline file for (no-baseline-scenario, test/model)".into(),
            },
        }];
        assert!(r.baseline_provenance.is_empty(), "test invariant");
        let out = r.render_text(false);
        assert!(
            out.contains("no scenarios graded against any baseline"),
            "expected empty-provenance diagnostic; got:\n{out}"
        );
        // The pointer at the Scenarios section is the recovery route —
        // make sure the wording stays aligned with the section heading.
        assert!(
            out.contains("Scenarios section below"),
            "expected pointer at per-scenario reasons; got:\n{out}"
        );
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

    #[test]
    fn render_text_verbose_surfaces_per_scenario_provenance() {
        // The "varied refs — see --verbose" hint emitted by the
        // metadata block when scenarios pin to different baseline
        // git_refs needs an actual verbose payload to point at.
        // Pin: each Graded scenario in --verbose output carries its
        // baseline's frozen_at + git_ref.
        let mut r = empty_report();
        r.headline_totals = ReportTotals {
            current_wins: 1,
            baseline_wins: 0,
            ties: 0,
        };
        r.scenarios = vec![
            ScenarioReport {
                scenario: "scenario-a".into(),
                outcome: ScenarioOutcome::Graded {
                    per_run_scores: vec![vec![]],
                    per_axis: Vec::new(),
                    errored_runs: Default::default(),
                },
            },
            ScenarioReport {
                scenario: "scenario-b".into(),
                outcome: ScenarioOutcome::Graded {
                    per_run_scores: vec![vec![]],
                    per_axis: Vec::new(),
                    errored_runs: Default::default(),
                },
            },
        ];
        r.baseline_provenance.insert(
            "scenario-a".into(),
            BaselineProvenance {
                git_ref: "ref-aaaa".into(),
                frozen_at: "2026-05-01T00:00:00Z".into(),
            },
        );
        r.baseline_provenance.insert(
            "scenario-b".into(),
            BaselineProvenance {
                git_ref: "ref-bbbb".into(),
                frozen_at: "2026-05-02T00:00:00Z".into(),
            },
        );
        let out = r.render_text(true);
        assert!(
            out.contains("ref-aaaa"),
            "missing scenario-a git_ref; got:\n{out}"
        );
        assert!(
            out.contains("ref-bbbb"),
            "missing scenario-b git_ref; got:\n{out}"
        );
        assert!(
            out.contains("2026-05-01"),
            "missing scenario-a frozen_at; got:\n{out}"
        );
        assert!(
            out.contains("2026-05-02"),
            "missing scenario-b frozen_at; got:\n{out}"
        );
    }
}
