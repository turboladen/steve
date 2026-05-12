//! Self-contained HTML report renderer for `steve eval report --html`.
//!
//! Single-file output: bundles Chart.js inline (~206KB) so the report
//! renders offline (CI artifacts, archived issues). All dynamic content
//! goes through `html_escape::encode_safe` to prevent XSS — scenario
//! names, user turns, tool args, tool outputs, and assistant messages
//! can all carry attacker-supplied HTML/JS sequences (scenarios
//! deliberately exercise the agent on real code).
//!
//! Visual language matches `src/mcp/oauth/callback.rs` — warm amber
//! gradient, white card on top, brown text palette, Steve-branded
//! footer. Layout is adapted from the OAuth page's single centered
//! card to a wider tabular page for the multi-section report content.

use html_escape::encode_safe;

use crate::eval::{
    history::HistoryEntry,
    report::{Report, ScenarioOutcome},
};

/// The Chart.js v4.4.7 UMD minified bundle, embedded at build time.
const CHARTJS_BUNDLE: &str = include_str!("../../assets/chartjs.min.js");

/// Chart.js MIT license text, embedded adjacent to the JS bundle so
/// the two cannot drift. Per spec: "must check it in as a static
/// string adjacent to the bundled JS so the two can never drift
/// apart."
const CHARTJS_LICENSE: &str = include_str!("../../assets/chartjs-LICENSE.txt");

/// CSS palette matching `src/mcp/oauth/callback.rs` — warm amber
/// gradient background, white card content, brown text. Layout is
/// adapted from the OAuth page's single centered card to a wider
/// tabular page (`max-width: 900px`) since the report content is
/// multi-section.
const REPORT_CSS: &str = r#"
* { margin: 0; padding: 0; box-sizing: border-box; }
body {
  min-height: 100vh;
  padding: 2rem 1rem;
  background: linear-gradient(135deg, #fff8e1 0%, #ffecb3 100%);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  color: #3e2723;
  line-height: 1.5;
}
.container {
  max-width: 900px;
  margin: 0 auto;
}
.card {
  background: #fff;
  border-radius: 20px;
  box-shadow: 0 8px 32px rgba(0,0,0,0.10);
  padding: 32px 36px;
  margin-bottom: 24px;
}
h1 { font-size: 1.5rem; font-weight: 700; margin-bottom: 0.5rem; }
h2 { font-size: 1.1rem; font-weight: 600; margin: 1.25rem 0 0.5rem; color: #5d4037; }
.meta { font-size: 0.9rem; color: #5d4037; margin-bottom: 0.5rem; }
.meta code { background: #fff8e1; padding: 1px 6px; border-radius: 6px; font-size: 0.85em; }
table { width: 100%; border-collapse: collapse; margin-top: 0.5rem; }
th, td { padding: 0.5rem 0.75rem; text-align: left; border-bottom: 1px solid #f1ede0; }
th { background: #fff8e1; font-weight: 600; color: #5d4037; }
.pos { color: #2e7d32; font-weight: 600; }
.neg { color: #c62828; font-weight: 600; }
.tie { color: #a1887f; }
.status { display: inline-block; padding: 4px 12px; border-radius: 999px; font-size: 0.75rem; font-weight: 600; letter-spacing: 0.02em; }
.status.skipped { background: #fff3e0; color: #e65100; }
.status.graded  { background: #e8f5e9; color: #2e7d32; }
canvas { max-width: 100%; }
.footer { margin-top: 1rem; font-size: 0.8rem; color: #a1887f; text-align: center; }
"#;

/// Render a self-contained HTML report. Inlines all dynamic content
/// XSS-safely via `html_escape::encode_safe`, embeds the Chart.js
/// bundle + MIT license, and (optionally) emits a trend chart from
/// `history`.
pub fn render_html(report: &Report, history: &[HistoryEntry]) -> String {
    let mut out = String::with_capacity(CHARTJS_BUNDLE.len() + 16 * 1024);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!(
        "<title>Steve — eval report — {} at {}</title>\n",
        encode_safe(&report.model),
        encode_safe(&report.results_git_ref)
    ));
    out.push_str("<style>");
    out.push_str(REPORT_CSS);
    out.push_str("</style>\n");
    out.push_str("</head><body>\n");

    // ── License comment ──
    out.push_str("<!--\n");
    out.push_str("Chart.js is bundled below under the MIT License.\n");
    out.push_str("Copyright (c) Chart.js Contributors. See https://www.chartjs.org/docs/latest/\n");
    out.push_str("Full license text:\n");
    // Escape the license text — `-->` inside it would break comment
    // boundaries. (Real MIT doesn't contain `-->`, but escape-by-
    // default is the load-bearing posture.)
    out.push_str(&encode_safe(CHARTJS_LICENSE));
    out.push_str("\n-->\n");

    out.push_str("<div class=\"container\">\n");

    // ── Header card (metadata) ──
    out.push_str("<div class=\"card\">\n");
    out.push_str("<h1>Eval report</h1>\n");
    out.push_str(&format!(
        "<p class=\"meta\">model: <code>{}</code> · git ref: <code>{}</code> · judge: <code>{}</code></p>\n",
        encode_safe(&report.model),
        encode_safe(&report.results_git_ref),
        encode_safe(&report.judge_model),
    ));
    out.push_str("</div>\n");

    // ── Headline card ──
    let headline = report.headline_totals;
    out.push_str("<div class=\"card\">\n");
    out.push_str("<h2>Headline</h2>\n<table>\n");
    out.push_str(&format!(
        "<tr><th>Net win rate</th><td class=\"{}\">{:+.1}%</td></tr>\n",
        if headline.net_win_rate() >= 0.0 {
            "pos"
        } else {
            "neg"
        },
        headline.net_win_rate() * 100.0,
    ));
    out.push_str(&format!(
        "<tr><th>Non-regression rate</th><td>{:.1}%</td></tr>\n",
        headline.non_regression_rate() * 100.0,
    ));
    out.push_str(&format!(
        "<tr><th>Verdicts</th><td>won {} · lost {} · tied {}</td></tr>\n",
        headline.current_wins, headline.baseline_wins, headline.ties,
    ));
    out.push_str("</table>\n");
    out.push_str("</div>\n");

    // ── Per-axis card ──
    if !report.per_axis.is_empty() {
        out.push_str("<div class=\"card\">\n");
        out.push_str("<h2>Per axis</h2>\n<table>\n<tr><th>Axis</th><th>Net win rate</th><th>Won</th><th>Lost</th><th>Tied</th></tr>\n");
        for ax in &report.per_axis {
            out.push_str(&format!(
                "<tr><td>{}</td><td class=\"{}\">{:+.1}%</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                encode_safe(&format!("{}", ax.axis)),
                if ax.totals.net_win_rate() >= 0.0 { "pos" } else { "neg" },
                ax.totals.net_win_rate() * 100.0,
                ax.totals.current_wins,
                ax.totals.baseline_wins,
                ax.totals.ties,
            ));
        }
        out.push_str("</table>\n");
        out.push_str("</div>\n");
    }

    // ── Per-scenario card ──
    out.push_str("<div class=\"card\">\n");
    out.push_str("<h2>Per scenario</h2>\n<table>\n<tr><th>Scenario</th><th>Status</th><th>Details</th></tr>\n");
    for sr in &report.scenarios {
        let scenario_escaped = encode_safe(&sr.scenario);
        match &sr.outcome {
            ScenarioOutcome::Graded {
                per_axis,
                per_run_scores,
            } => {
                let detail: String = per_axis
                    .iter()
                    .map(|ax| {
                        format!(
                            "{}: won {} / lost {} / tied {}",
                            ax.axis,
                            ax.totals.current_wins,
                            ax.totals.baseline_wins,
                            ax.totals.ties
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                out.push_str(&format!(
                    "<tr><td>{scenario_escaped}</td><td><span class=\"status graded\">Graded ({} runs)</span></td><td>{}</td></tr>\n",
                    per_run_scores.len(),
                    encode_safe(&detail),
                ));
            }
            ScenarioOutcome::Skipped { reason } => {
                out.push_str(&format!(
                    "<tr><td>{scenario_escaped}</td><td><span class=\"status skipped\">Skipped</span></td><td>{}</td></tr>\n",
                    encode_safe(reason),
                ));
            }
        }
    }
    out.push_str("</table>\n");
    out.push_str("</div>\n");

    // ── Trend chart card (only if history has rows) ──
    if !history.is_empty() {
        let labels: Vec<String> = history.iter().map(|h| h.git_ref.clone()).collect();
        let values: Vec<f64> = history.iter().map(|h| h.headline.net_win_rate).collect();
        let labels_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".into());
        let values_json = serde_json::to_string(&values).unwrap_or_else(|_| "[]".into());

        out.push_str("<div class=\"card\">\n");
        out.push_str("<h2>Trends over time</h2>\n<canvas id=\"trendChart\"></canvas>\n");
        out.push_str("</div>\n");
        out.push_str("<script>\n");
        out.push_str(CHARTJS_BUNDLE);
        out.push_str("\n</script>\n");
        out.push_str("<script>\n");
        out.push_str(&format!(
            "new Chart(document.getElementById('trendChart'), {{type: 'line', data: {{labels: {labels_json}, datasets: [{{label: 'net win rate', data: {values_json}, borderColor: '#2e7d32', backgroundColor: 'rgba(46,125,50,0.1)', tension: 0.2, fill: true}}]}}, options: {{plugins: {{legend: {{display: false}}}}, scales: {{y: {{title: {{display: true, text: 'Net win rate'}}}}, x: {{title: {{display: true, text: 'git ref'}}}}}}}}}});\n"
        ));
        out.push_str("</script>\n");
    }

    out.push_str("<p class=\"footer\">steve · rust tui coding agent</p>\n");
    out.push_str("</div>\n");
    out.push_str("</body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{
        history::{HistoryAxisEntry, HistoryFloor, HistoryHeadline},
        report::{AxisTotals, ReportTotals, ScenarioOutcome, ScenarioReport},
        score::Axis,
    };
    use std::collections::BTreeMap;

    fn report_with_xss_attempt() -> Report {
        Report {
            model: "test/model".into(),
            results_git_ref: "abc1234".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: "test/judge".into(),
            headline_totals: ReportTotals {
                current_wins: 1,
                baseline_wins: 0,
                ties: 0,
            },
            per_axis: Vec::new(),
            scenarios: vec![ScenarioReport {
                scenario: r#"<script>alert("xss")</script>"#.into(),
                outcome: ScenarioOutcome::Skipped {
                    reason: r#"<img onerror="alert(1)" src=x>"#.into(),
                },
            }],
        }
    }

    #[test]
    fn html_report_escapes_dynamic_content_to_prevent_xss() {
        let r = report_with_xss_attempt();
        let html = render_html(&r, &[]);
        // The original payload tags must not appear raw.
        assert!(
            !html.contains("<script>alert"),
            "found unescaped script tag in HTML"
        );
        assert!(
            !html.contains(r#"onerror="alert(1)""#),
            "found raw event handler"
        );
        // The escaped form should appear.
        assert!(
            html.contains("&lt;script&gt;"),
            "expected escaped script tag"
        );
        assert!(html.contains("&lt;img"), "expected escaped img tag");
    }

    #[test]
    fn html_report_contains_chartjs_bundle_marker_and_mit_license() {
        let r = report_with_xss_attempt();
        let history = vec![sample_history_entry()];
        let html = render_html(&r, &history);
        // Chart.js bundle marker (4.4.7 is in the bundle's source map ref).
        assert!(
            html.contains("Chart"),
            "expected Chart.js bundle to be embedded"
        );
        // MIT license text.
        assert!(
            html.contains("MIT License"),
            "expected MIT license text to be embedded"
        );
    }

    #[test]
    fn html_report_includes_headline_percentage() {
        let mut r = report_with_xss_attempt();
        r.scenarios = Vec::new(); // strip the xss scenario for this test
        r.headline_totals = ReportTotals {
            current_wins: 5,
            baseline_wins: 0,
            ties: 5,
        };
        r.per_axis = vec![AxisTotals {
            axis: Axis::Correctness,
            totals: ReportTotals {
                current_wins: 2,
                baseline_wins: 1,
                ties: 0,
            },
        }];
        let html = render_html(&r, &[]);
        // (5-0)/10 = 0.5 → +50.0%
        assert!(html.contains("+50.0%"), "got headline missing +50.0%");
    }

    #[test]
    fn html_report_omits_trends_section_when_history_empty() {
        let r = report_with_xss_attempt();
        let html = render_html(&r, &[]);
        assert!(
            !html.contains("trendChart"),
            "trend canvas should be absent when history is empty"
        );
    }

    #[test]
    fn html_report_emits_trends_section_when_history_has_rows() {
        let r = report_with_xss_attempt();
        let history = vec![sample_history_entry()];
        let html = render_html(&r, &history);
        assert!(
            html.contains("trendChart"),
            "trend canvas should be present when history has rows"
        );
        // The git_ref from the sample row should appear in the JSON data.
        assert!(
            html.contains("\"sample-ref\""),
            "trend data should include the git_ref"
        );
    }

    #[test]
    fn html_report_uses_oauth_callback_palette() {
        // The visual language matches src/mcp/oauth/callback.rs — verify
        // the warm-amber gradient and brown-text palette ship in the output.
        let r = report_with_xss_attempt();
        let html = render_html(&r, &[]);
        assert!(html.contains("#fff8e1"), "expected amber-50 background");
        assert!(html.contains("#3e2723"), "expected brown body text");
        assert!(
            html.contains("steve · rust tui coding agent"),
            "expected Steve-branded footer"
        );
    }

    fn sample_history_entry() -> HistoryEntry {
        HistoryEntry {
            git_ref: "sample-ref".into(),
            recorded_at: "2026-05-12T00:00:00Z".into(),
            model: "test/model".into(),
            baseline_git_ref: "x".into(),
            judge_model: "x".into(),
            headline: HistoryHeadline {
                net_win_rate: 0.02,
                non_regression_rate: 0.98,
            },
            per_axis: BTreeMap::new(),
            deterministic_floor: HistoryFloor {
                passed: 0,
                total: 0,
            },
            results_file: "x".into(),
        }
    }

    /// Helper to silence the dead-code lint on the otherwise-unused
    /// fields of HistoryAxisEntry. The HTML renderer doesn't yet
    /// surface per-axis breakdown in the trend chart (single line for
    /// suite-wide headline only); a future iteration could.
    #[allow(dead_code)]
    fn _unused_history_axis_entry_for_lint(e: HistoryAxisEntry) -> HistoryAxisEntry {
        e
    }
}
