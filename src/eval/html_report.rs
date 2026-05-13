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
    // Capacity: only reserve the Chart.js bundle's ~200KB when we'll
    // actually embed it. Empty-history reports skip the bundle and
    // license entirely (see below) so a 16KB headroom suffices.
    let bundle_size = if history.is_empty() {
        0
    } else {
        CHARTJS_BUNDLE.len()
    };
    let mut out = String::with_capacity(bundle_size + 16 * 1024);
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

    out.push_str("<div class=\"container\">\n");

    // ── Header card (metadata) ──
    out.push_str("<div class=\"card\">\n");
    out.push_str("<h1>Eval report</h1>\n");
    let judge_display = report
        .judge_model
        .as_deref()
        .unwrap_or("per-scenario (see scenario.toml)");
    out.push_str(&format!(
        "<p class=\"meta\">model: <code>{}</code> · git ref: <code>{}</code> · judge: <code>{}</code></p>\n",
        encode_safe(&report.model),
        encode_safe(&report.results_git_ref),
        encode_safe(judge_display),
    ));
    // Baseline provenance row — mirrors the text renderer's metadata
    // block. Same three cases: single ref (one frozen_at + git_ref),
    // varied refs (count + reference to per-scenario card below),
    // none (no Graded scenarios so no baselines anchored).
    if let Some(first) = report.baseline_provenance.values().next() {
        let all_same = report
            .baseline_provenance
            .values()
            .all(|p| p.git_ref == first.git_ref && p.frozen_at == first.frozen_at);
        let scenario_count = report.baseline_provenance.len();
        if all_same {
            out.push_str(&format!(
                "<p class=\"meta\">baseline: frozen <code>{}</code> at <code>{}</code> ({} scenarios)</p>\n",
                encode_safe(&first.frozen_at),
                encode_safe(&first.git_ref),
                scenario_count,
            ));
        } else {
            out.push_str(&format!(
                "<p class=\"meta\">baselines: {scenario_count} scenarios, varied refs (per-scenario detail below)</p>\n",
            ));
        }
    }
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
                errored_runs,
            } => {
                let mut detail: String = per_axis
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
                // When runs errored on both judge-call attempts, surface
                // the last error message in the detail column so the
                // HTML dashboard isn't worse than the CLI --verbose
                // output. (Per spec, HTML IS the dashboard operators
                // primarily look at; losing the failure diagnostic
                // here would force them to re-run with --verbose just
                // to figure out which knob to turn.)
                if let Some(err) = &errored_runs.last_error {
                    if !detail.is_empty() {
                        detail.push_str(" — ");
                    }
                    detail.push_str(&format!("last error: {err}"));
                }
                // Per-scenario baseline provenance — when scenarios
                // pin to different baseline refs, the header's
                // "varied refs (per-scenario detail below)" hint
                // points here. Skipped scenarios have no provenance
                // (no baseline to anchor against), so this only
                // appears for Graded rows.
                if let Some(prov) = report.baseline_provenance.get(&sr.scenario) {
                    if !detail.is_empty() {
                        detail.push_str(" — ");
                    }
                    detail.push_str(&format!(
                        "baseline: frozen {} at {}",
                        prov.frozen_at, prov.git_ref
                    ));
                }
                // Surface errored runs in the status pill so the
                // operator notices sample-size shrinkage (graded
                // count < total runs) instead of reading a clean
                // "Graded (N runs)" pill that silently dropped
                // errored samples.
                let status_label = if errored_runs.count > 0 {
                    format!(
                        "Graded ({} runs, {} errored)",
                        per_run_scores.len(),
                        errored_runs.count
                    )
                } else {
                    format!("Graded ({} runs)", per_run_scores.len())
                };
                out.push_str(&format!(
                    "<tr><td>{scenario_escaped}</td><td><span class=\"status graded\">{}</span></td><td>{}</td></tr>\n",
                    encode_safe(&status_label),
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
        // serde_json refuses to serialize NaN/Infinity (JSON spec rejects
        // them) — a non-finite net_win_rate would otherwise collapse the
        // entire trend chart to "[]" via the fallback path. Drop the
        // offending pair (label, value) and surface to stderr so the
        // operator sees signal instead of a blank chart. tracing is
        // file-only in this binary, hence the bare eprintln.
        let mut labels: Vec<String> = Vec::with_capacity(history.len());
        let mut values: Vec<f64> = Vec::with_capacity(history.len());
        for h in history {
            if h.headline.net_win_rate.is_finite() {
                labels.push(h.git_ref.clone());
                values.push(h.headline.net_win_rate);
            } else {
                eprintln!(
                    "warning: dropping non-finite net_win_rate from trend chart (git_ref={})",
                    h.git_ref
                );
            }
        }
        let labels_json =
            escape_json_for_script(&serde_json::to_string(&labels).unwrap_or_else(|_| "[]".into()));
        let values_json =
            escape_json_for_script(&serde_json::to_string(&values).unwrap_or_else(|_| "[]".into()));

        out.push_str("<div class=\"card\">\n");
        out.push_str("<h2>Trends over time</h2>\n<canvas id=\"trendChart\"></canvas>\n");
        out.push_str("</div>\n");

        // License comment lives next to the bundle it covers — when
        // history is empty, we skip both, so the comment never
        // misrepresents the page contents. Defend against `-->`
        // inside the embedded license terminating the wrapping HTML
        // comment. `encode_safe` handles `<` / `>` / `&` / quotes
        // but does NOT escape the `-->` sequence — apply a separate
        // replacement so the comment boundary is reliable even if a
        // future replacement license contains the sequence.
        out.push_str("<!--\n");
        out.push_str("Chart.js is bundled below under the MIT License.\n");
        out.push_str(
            "Copyright (c) Chart.js Contributors. See https://www.chartjs.org/docs/latest/\n",
        );
        out.push_str("Full license text:\n");
        let escaped_license = encode_safe(CHARTJS_LICENSE).replace("-->", "--&gt;");
        out.push_str(&escaped_license);
        out.push_str("\n-->\n");

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

/// Re-encode `<`, `>`, `&`, U+2028, and U+2029 in a JSON string as
/// their `\uXXXX` JSON escapes. The result is still valid JSON
/// (JSON spec accepts these chars in strings) and parses
/// identically — but the characters that could terminate an HTML
/// `<script>` block, introduce an HTML entity, or (pre-ES2019)
/// terminate a JS string literal are now neutralized, so it's safe
/// to interpolate the result inline inside a `<script>` body.
///
/// Without this, a `git_ref` containing the literal `</script>`
/// would break out of the wrapping `<script>` tag. `serde_json` by
/// default does NOT escape these characters because they're valid
/// inside JSON string values; the HTML-in-script context is where
/// they become dangerous.
///
/// U+2028 (LINE SEPARATOR) and U+2029 (PARAGRAPH SEPARATOR) are
/// included for parity with the canonical OWASP / Rails `json_escape`
/// helper. ES2019 normalized them as JS whitespace (modern engines
/// don't treat them as statement terminators), but escaping them
/// keeps the helper robust against archaic engines and unknown
/// downstream tooling.
fn escape_json_for_script(json: &str) -> String {
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{
        history::{HistoryAxisEntry, HistoryFloor, HistoryHeadline},
        report::{AxisTotals, BaselineProvenance, ReportTotals, ScenarioOutcome, ScenarioReport},
        score::Axis,
    };
    use std::collections::BTreeMap;

    fn report_with_xss_attempt() -> Report {
        Report {
            model: "test/model".into(),
            results_git_ref: "abc1234".into(),
            results_path: "results.yaml".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: Some("test/judge".into()),
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
            deterministic_floor: Default::default(),
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
    fn html_report_omits_chartjs_bundle_and_license_when_history_empty() {
        // The Chart.js bundle (~200KB) AND its MIT license comment
        // are coupled to the trend chart — both ride together or
        // neither does. When history is empty, the trend chart is
        // omitted, so the bundle is dead weight and the license
        // comment ("Chart.js is bundled below") misrepresents
        // the page. Both must be gated on `!history.is_empty()`.
        let r = report_with_xss_attempt();
        let html = render_html(&r, &[]);
        assert!(
            !html.contains("MIT License"),
            "MIT license must be omitted when no Chart.js bundle is emitted"
        );
        assert!(
            !html.contains("Chart.js is bundled below"),
            "license comment must be omitted when no bundle is emitted"
        );
        // Sanity: the page is much smaller without the bundle than
        // with it. Bundle alone is ~200KB; report HTML without it
        // should comfortably fit in under 50KB.
        assert!(
            html.len() < 50_000,
            "empty-history report should not allocate the Chart.js bundle (got {} bytes)",
            html.len()
        );
    }

    #[test]
    fn html_report_neutralizes_script_breakout_in_trend_chart_json() {
        // Pin: serde_json::to_string doesn't escape `<`, `>`, `&`,
        // so a `git_ref = "</script>..."` would break out of the
        // wrapping <script> tag. The other XSS test covers the
        // per-scenario Skipped path, not the trend chart JSON.
        let r = report_with_xss_attempt();
        let mut payload = sample_history_entry();
        payload.git_ref = "</script><script>alert(1)//".into();
        let history = vec![payload];
        let html = render_html(&r, &history);
        // Raw payload must NOT appear in the output.
        assert!(
            !html.contains("</script><script>alert(1)//"),
            "script-breakout payload appeared raw in HTML"
        );
        // The escaped form should appear instead. Both `<` and `>`
        // in the payload must be Unicode-escaped (`<` /
        // `>`) inside the trend chart's JSON array.
        assert!(
            html.contains("\\u003c") && html.contains("\\u003e"),
            "expected `\\u003c` / `\\u003e` escapes in trend-chart JSON"
        );
    }

    #[test]
    fn html_report_drops_non_finite_history_rows_from_trend_chart() {
        // A HistoryEntry with NaN/Infinity net_win_rate would cause
        // serde_json::to_string to error, collapsing the entire chart
        // to "[]" via the unwrap_or_else fallback — silently hiding
        // every other row. Filter the offender, keep the rest.
        let r = report_with_xss_attempt();
        let mut good = sample_history_entry();
        good.git_ref = "good-ref".into();
        good.headline.net_win_rate = 0.5;
        let mut nan = sample_history_entry();
        nan.git_ref = "nan-ref".into();
        nan.headline.net_win_rate = f64::NAN;
        let html = render_html(&r, &[good, nan]);
        assert!(
            html.contains("good-ref"),
            "finite row must survive the filter"
        );
        assert!(
            !html.contains("nan-ref"),
            "non-finite row must be dropped from the chart"
        );
        assert!(html.contains("0.5"), "finite value must reach the chart");
    }

    #[test]
    fn html_report_neutralizes_line_separators_in_trend_chart_json() {
        // Mirror of the script-breakout test but for U+2028/U+2029.
        // The unit test on the helper alone can't catch a future
        // render-path refactor that swapped serde_json::to_string for
        // a hand-rolled formatter — pin the end-to-end render output.
        let r = report_with_xss_attempt();
        let mut payload = sample_history_entry();
        payload.git_ref = "evil\u{2028}; alert(1);".into();
        let html = render_html(&r, &[payload]);
        assert!(
            !html.contains('\u{2028}'),
            "raw U+2028 must not survive into rendered HTML"
        );
        assert!(
            html.contains("\\u2028"),
            "expected \\u2028 escape in trend-chart JSON"
        );
    }

    #[test]
    fn escape_json_for_script_neutralizes_line_separators() {
        // U+2028 / U+2029 were JS statement terminators pre-ES2019.
        // Modern engines treat them as whitespace, but the canonical
        // OWASP / Rails json_escape helper includes them and they're
        // cheap to encode. Pin the contract so a future "simpler"
        // refactor doesn't drop them.
        //
        // Run on the REAL call path: serde_json::to_string produces
        // the JSON, escape_json_for_script post-processes it, then
        // the result must still parse as JSON. Catches a hypothetical
        // future helper that operated on byte ranges instead of
        // codepoints and broke UTF-8 invariants.
        let json = serde_json::to_string(&"\u{2028}\u{2029}").unwrap();
        let out = escape_json_for_script(&json);
        assert!(
            !out.contains('\u{2028}') && !out.contains('\u{2029}'),
            "expected line-separator chars to be escaped; got: {out:?}"
        );
        assert!(
            out.contains("\\u2028") && out.contains("\\u2029"),
            "expected \\u2028 / \\u2029 JSON escapes; got: {out:?}"
        );
        // Result must still be valid JSON — JSON spec accepts \uXXXX.
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("escaped output must remain valid JSON");
        // And must round-trip back to the original chars.
        assert_eq!(parsed.as_str().unwrap(), "\u{2028}\u{2029}");
    }

    #[test]
    fn escape_json_for_script_neutralizes_three_dangerous_chars() {
        // Plain strings pass through unchanged.
        let plain = escape_json_for_script("\"hello\"");
        assert_eq!(plain, "\"hello\"");

        // `<` becomes the 6-char escape `<` and `>` becomes
        // `>`. After replacement, the raw `<` / `>` chars
        // must not appear in the output (the whole point — they're
        // what can break out of a <script> tag).
        let dangerous = escape_json_for_script("\"</script>\"");
        assert!(
            !dangerous.contains('<'),
            "expected `<` to be escaped; got: {dangerous}"
        );
        assert!(
            !dangerous.contains('>'),
            "expected `>` to be escaped; got: {dangerous}"
        );
        assert!(
            dangerous.contains("\\u003c"),
            "expected `\\u003c` escape for `<`; got: {dangerous}"
        );
        assert!(
            dangerous.contains("\\u003e"),
            "expected `\\u003e` escape for `>`; got: {dangerous}"
        );

        // `&` becomes `&`.
        let amp = escape_json_for_script("\"a & b\"");
        assert!(!amp.contains('&'), "expected `&` to be escaped; got: {amp}");
        assert!(
            amp.contains("\\u0026"),
            "expected `\\u0026` escape for `&`; got: {amp}"
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
    fn html_report_surfaces_errored_runs_in_status_pill_and_last_error_in_detail() {
        // The HTML status pill is the operator-visible "sample size
        // shrunk on you" signal. Spec: HTML is the dashboard you
        // actually look at — losing the failure diagnostic here would
        // force operators back to CLI --verbose just to figure out
        // which knob to turn. Pin BOTH the status pill count AND the
        // last_error text in the detail column.
        let mut r = report_with_xss_attempt();
        r.scenarios = vec![ScenarioReport {
            scenario: "shrunk".into(),
            outcome: ScenarioOutcome::Graded {
                per_run_scores: vec![vec![]],
                per_axis: Vec::new(),
                errored_runs: crate::eval::report::ErroredRuns {
                    count: 2,
                    last_error: Some("simulated 503".into()),
                },
            },
        }];
        let html = render_html(&r, &[]);
        assert!(
            html.contains("Graded (1 runs, 2 errored)"),
            "expected status pill to surface errored count"
        );
        // The last_error message must appear in the HTML so operators
        // see the failure diagnostic without dropping to CLI.
        assert!(
            html.contains("simulated 503"),
            "expected last_error to be surfaced in HTML detail"
        );
        assert!(
            html.contains("last error:"),
            "expected 'last error:' label so the diagnostic is clearly identified"
        );
    }

    #[test]
    fn html_report_shows_per_scenario_judge_when_judge_model_is_none() {
        // judge_model = None must render "per-scenario" prose, never
        // the literal "<per-scenario>" placeholder — the latter
        // poisons downstream provider/model parsers in history.jsonl.
        let mut r = report_with_xss_attempt();
        r.judge_model = None;
        let html = render_html(&r, &[]);
        assert!(
            !html.contains("<per-scenario>"),
            "old placeholder string must not leak into HTML"
        );
        assert!(
            html.contains("per-scenario"),
            "expected 'per-scenario' prose in the judge metadata"
        );
    }

    #[test]
    fn html_report_header_shows_baseline_provenance_when_single_ref() {
        let mut r = report_with_xss_attempt();
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
        let html = render_html(&r, &[]);
        assert!(
            html.contains("abc1234"),
            "header must surface single-ref baseline git_ref"
        );
        assert!(
            html.contains("2026-05-01T00:00:00Z"),
            "header must surface single-ref frozen_at"
        );
        assert!(
            html.contains("(2 scenarios)"),
            "header must surface scenario count"
        );
        // Should NOT mention varied-refs framing in the single-ref case.
        assert!(
            !html.contains("varied refs"),
            "single-ref baseline must not use 'varied refs' framing"
        );
    }

    #[test]
    fn html_report_header_surfaces_varied_baseline_refs() {
        // Mirror of the text renderer's "varied refs" hint. When
        // scenarios pin to different baseline git_refs, the header
        // says so and points readers at the per-scenario table for
        // detail. Otherwise the operator has no way to spot baseline
        // drift between scenarios from the dashboard alone.
        let mut r = report_with_xss_attempt();
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
        let html = render_html(&r, &[]);
        assert!(
            html.contains("varied refs"),
            "header must signal varied baseline refs"
        );
    }

    #[test]
    fn html_report_per_scenario_table_surfaces_baseline_provenance() {
        // The header's "varied refs (per-scenario detail below)"
        // hint needs to actually deliver per-scenario provenance.
        // Pin: each Graded row's Details column includes the
        // baseline's frozen_at + git_ref.
        let mut r = empty_report_for_provenance_tests();
        r.scenarios = vec![ScenarioReport {
            scenario: "scenario-x".into(),
            outcome: ScenarioOutcome::Graded {
                per_run_scores: vec![vec![]],
                per_axis: Vec::new(),
                errored_runs: Default::default(),
            },
        }];
        r.baseline_provenance.insert(
            "scenario-x".into(),
            BaselineProvenance {
                git_ref: "ref-xxxx".into(),
                frozen_at: "2026-05-03T00:00:00Z".into(),
            },
        );
        let html = render_html(&r, &[]);
        assert!(
            html.contains("ref-xxxx"),
            "per-scenario row must show git_ref"
        );
        assert!(
            html.contains("2026-05-03"),
            "per-scenario row must show frozen_at"
        );
    }

    #[test]
    fn html_report_varied_refs_header_and_per_scenario_rows_render_together() {
        // The header's "varied refs (per-scenario detail below)"
        // hint is only useful if the per-scenario card actually
        // surfaces each scenario's distinct ref. Pin BOTH halves in
        // a single render — exercising the case the two existing
        // sibling tests each cover in isolation but never together.
        let mut r = empty_report_for_provenance_tests();
        r.headline_totals = ReportTotals {
            current_wins: 2,
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
        let html = render_html(&r, &[]);
        // Header signals varied refs.
        assert!(html.contains("varied refs"), "expected varied-refs header");
        // Both per-scenario rows surface their respective provenance.
        let per_scenario_section = html
            .split("<h2>Per scenario</h2>")
            .nth(1)
            .expect("per-scenario card must be present");
        assert!(
            per_scenario_section.contains("ref-aaaa"),
            "scenario-a's git_ref must appear in the per-scenario card"
        );
        assert!(
            per_scenario_section.contains("ref-bbbb"),
            "scenario-b's git_ref must appear in the per-scenario card"
        );
    }

    fn empty_report_for_provenance_tests() -> Report {
        Report {
            model: "m".into(),
            results_git_ref: "g".into(),
            results_path: "p".into(),
            baseline_provenance: BTreeMap::new(),
            judge_model: None,
            headline_totals: ReportTotals::default(),
            per_axis: Vec::new(),
            scenarios: Vec::new(),
            deterministic_floor: Default::default(),
        }
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
            judge_model: Some("x".into()),
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
