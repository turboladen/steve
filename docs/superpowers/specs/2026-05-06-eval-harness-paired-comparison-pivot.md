# Eval Harness: Pivot from Pass/Fail to Paired-Comparison Scoring

> **Status:** Design approved 2026-05-06. Supersedes the original Phase 6
> (`steve-53nw`) scope. Phases 1–5 of the eval epic (`steve-ffdq`) ship as
> built; the redesign rescopes Phase 6 onward.

## Why pivot

The eval harness was built on a binary primitive: every scenario produces
`Outcome::Passed | Failed | Skipped`, every judge call returns
`JudgeVerdict::Passed | Failed`, and the suite's headline number is
"X/N scenarios passed." That shape works for narrow correctness invariants
("did the agent call this tool", "does the file end up containing X") but
is the wrong primitive for the user's actual goal:

> Gauge whether the app regresses overall after making changes — and tell
> how much it has improved as a whole.

Pass/fail aggregation only catches regressions large enough to flip a
scenario across whatever threshold was picked. Anything subtler — agent
gets more correct but less efficient, agent's responses become more
verbose without changing the tool sequence, agent picks a worse-but-still-
working tool — is invisible. For "is the app overall better/worse," you
need a *continuous* signal, not a discrete one.

The pivot replaces the headline metric (binary, lossy) with paired
comparison against a frozen baseline (continuous, sensitive). The existing
pass/fail assertions stay as a deterministic floor — they catch hard
correctness invariants regardless of judge opinion — but they stop being
the primary signal.

## Goals

1. **Sensitive regression detection.** Catch small regressions before they
   become full scenario failures.
2. **Cumulative improvement tracking.** Tell how much the app has improved
   versus a chosen anchor (a release tag, a date, a known-good commit).
3. **Cross-model comparison.** Run the same scenarios with different agent
   models (e.g., free local Ollama vs paid Fuel-IX-relayed Claude) and
   compare them on equal footing.

## Prior art / convention anchors

This design isn't modeled on a single framework — it's a hybrid of several
established patterns. Naming the lineage explicitly so future design
calls can be sanity-checked against it:

- **Chatbot Arena / LMSYS Arena** — pairwise-comparison + win-rate
  aggregation primitive. Originally inspired by chess Elo. The core idea
  ("compare two outputs, judge picks one, aggregate over many pairs") is
  theirs.
- **MT-Bench** and **G-Eval** — LLM-as-judge with structured multi-axis
  rubric. G-Eval specifically formalized the "chain-of-thought-rationale-
  before-verdict" prompt pattern that mitigates halo effect; our
  per-axis-rationale-before-verdict prompt design is borrowed directly.
- **Snapshot testing** (`insta` in Rust, `jest` snapshots, golden tests
  in compilers) — frozen baselines as committed text artifacts. The
  diff-baseline-in-PR workflow is theirs. Our `Normalizer` is what
  `insta` calls a redactor.
- **Inspect AI** (UK AISI's open eval framework) — decoupled run/report
  architecture and "logs are first-class replayable artifacts"
  philosophy. Their `Task` / `Solver` / `Scorer` decomposition closely
  mirrors our `Scenario` / `Runner` / `Judge` split. **Closest single
  existing analog** to what we're building. When in doubt about a future
  design call, "what would Inspect AI do here?" is a defensible
  sanity check — particularly their log-as-artifact philosophy, which
  is what makes "generate once, compare many times" work.
- **HELM** (Stanford, 2022) — "deterministic floor + graded layer"
  pattern. Narrow correctness checks AND graded scenarios, both
  reported, neither replacing the other.

**Explicit non-anchor:** we are not trying to BE Inspect AI. Their
scope is much broader (multi-task suites, agent harness scaffolding,
web viewer, HuggingFace/OpenAI eval format integrations). Steve's eval
is intentionally narrower — project-specific, CI-gated, plain-text
artifacts in git, no web UI. Cherry-pick ideas; don't import scope.

## Non-goals (deliberately deferred)

- **Elo / Glicko rating across many model versions.** The technically-
  correct answer for long-horizon many-version comparison, but overkill
  for current scale. File when there are >5 historical baselines worth
  tracking.
- **Adaptive multi-run** (run more samples until convergence). Adds
  cost-variability and aggregator complexity. Fixed N with per-scenario
  override is sufficient.
- **Judge-verdict caching** keyed on (transcript-hash, baseline-hash,
  judge-model, prompt-version). Useful optimization later; not required
  for v1.
- **Cumulative anchor baselines** (named slots like `v0.4.0` in the
  manifest). Not built until there's a second anchor to track. v1 has
  one baseline per (scenario, model); `git checkout` of older baseline
  files is the v1 mechanism for "compare against an older anchor."
- **Replacing the existing rule-based assertions.** They stay as the
  deterministic floor; their failures gate hard-correctness regressions
  that should never be masked by a judge's opinion.

## Architecture

### Three-stage data flow

```
[scenarios]  →  steve eval run     →  [results.yaml]
                                              ↓
[baseline.yaml]  ←  steve eval baseline freeze
                                              ↓
[results.yaml] + [baseline.yaml]  →  steve eval report  →  headline + per-axis + per-scenario
```

Three operations, three concerns:

1. **Run** — sample agent behavior. Reads scenarios, runs each K times,
   writes a normalized YAML results file. **Zero judge calls.**
2. **Freeze** — turn a run into a baseline. Conceptually `run + mv` into
   `eval/baselines/<scenario>/<model>.yaml`, plus a manifest entry.
3. **Report** — judge results against a baseline and emit the layered
   headline. **All judge calls happen here.**

`steve eval` (no args) chains run → report against the configured
baseline. The common case stays one command.

### Why decouple run from report

Three downstream wins:

- **Generate once, compare many times.** Agent runs are the expensive
  part. A 10-scenario suite with N=3 runs against Anthropic costs real
  money in tokens and time; judge calls are cheap (~$0.20). Decoupling
  means a single `steve eval run` can be reported against multiple
  baselines without burning more agent tokens.
- **Backtest judge changes.** Change the judge prompt or upgrade the
  judge model? Re-`report` against archived results files — past behavior
  gets re-graded under the new rubric. Calibration drift becomes visible
  and recoverable.
- **Cross-model compare for free.** "Compare ollama vs anthropic" is just
  `steve eval report ollama-results.yaml --baseline anthropic-results.yaml`.
  No new machinery: baselines and run-outputs share a schema, so any
  results file can serve as either side of a comparison.

The shared schema between baselines and run-outputs is the load-bearing
piece. A baseline IS a run-output that's been frozen.

### Baselines as files in git

Baselines live at `eval/baselines/<scenario>/<model_id>.yaml`, checked
into the repo. Format is plain YAML via serde-saphyr (see "File format"
below for why). A small `eval/baselines/manifest.toml` records provenance:

```toml
[[baseline]]
scenario = "no-hallucinated-tool-output"
model = "ollama/qwen3-coder"
git_ref = "abc1234"
frozen_at = "2026-05-06"
judge_model = "fuel-ix/claude-haiku-4-5"
```

Refresh is explicit: `steve eval baseline freeze --scenario X --model Y`
re-runs and overwrites. The manifest entry updates atomically with the
file.

This shape buys:

- **Shared across machines.** Anyone who clones the repo can run evals
  immediately.
- **Reproducible.** `git checkout v0.4.0` includes the v0.4.0 baselines.
  No external sync needed.
- **PR-reviewable.** A baseline-refresh PR shows the diff. Reviewers see
  *what behavior changed*, not just that it changed.
- **`git blame`-able.** "When did the agent stop doing X?" answers via
  history, not a separate database.

### Per-model is non-negotiable

Comparing "ollama with new system prompt" against an Anthropic baseline
tells you nothing about whether your prompt change regressed — it tells
you how ollama compares to Anthropic, a different question. Baselines
must match what they're being compared against.

## File format: YAML via serde-saphyr

Plain pretty-printed YAML, NOT gzipped. Reasons:

- Tool results, assistant messages, and user turns contain multi-line
  content (code blocks, file contents, paragraphs). YAML's `|` block
  scalar preserves literal newlines; JSON escapes them to `\n` strings,
  destroying line-level diffability.
- YAML's nested-event ergonomics beat TOML's table-fragmentation when
  events have nested args (e.g., `EditOperation` variants).
- Plain text files give git everything it's good at — diffs, blame,
  grep across history.

`serde_yaml` has been unmaintained since 2024. We use `serde-saphyr` as
the active fork. The format choice is reversible: `Baseline:
Serialize/Deserialize` keeps it a few-line change to swap libraries.

Repo size budget: ~50KB raw YAML per (scenario, model) baseline. At
10 scenarios × 3 models = ~1.5MB total. At 50 scenarios × 5 models =
~12.5MB. Git handles single-digit MB without issue. If/when this becomes
a problem, transcript pruning (drop tool-result bodies that aren't load-
bearing for the judge) is the first lever.

## Schema

### Normalizer

`CapturedRun` carries fields that are noise for diffs and baselines:
exact timestamps, full duration in nanoseconds, workspace tempdir paths
(UUID-bearing), tool-call UUIDs. A `Normalizer` helper takes a
`CapturedRun` and produces a baseline-shaped struct that strips or
canonicalizes those:

- Timestamps removed (sequence is implied by array order)
- Workspace paths normalized to relative form rooted at the scenario
  fixture root
- Tool-call UUIDs dropped
- Token counts kept (informational; tracked across runs as a usage signal)
- Duration kept but rounded to whole milliseconds (jitter-friendly)

Used at two boundaries:

1. **Freeze time:** writes the normalized form to baseline files.
2. **Report time:** normalizes the live `CapturedRun` before passing to
   the judge so comparison is apples-to-apples.

### New `Score` channel alongside existing `Outcome`

The existing `Outcome::Passed | Failed | Skipped` enum stays. The
existing rule-based assertions in `expectations.rs` stay. They become
the *deterministic floor* — hard correctness invariants that gate
"this transcript is even valid for comparison."

A new parallel channel carries paired-comparison scores:

```rust
pub enum Axis {
    Correctness,
    Efficiency,
    Conciseness,
    // Additional axes added by enum variant when there's a concrete
    // use case. Arbitrary user-defined axis names are deferred —
    // the per-scenario override (below) parses into this enum, so
    // a typo in scenario.toml fails at load time rather than
    // silently producing an unknown-axis judge prompt.
}

pub enum Verdict { CurrentWins, BaselineWins, Tie }

pub struct PairedScore {
    pub axis: Axis,
    pub verdict: Verdict,
    pub rationale: String,    // judge's per-axis justification
}

pub struct ScenarioScore {
    pub scenario: String,
    pub model: String,
    pub run_index: usize,     // which of the N runs this is
    pub deterministic_floor_passed: bool,  // from existing Outcome channel
    pub axes: Vec<PairedScore>,
}

pub struct NormalizedTranscript {
    pub scenario_name: String,
    pub model: String,
    pub git_ref: Option<String>,    // populated for baseline files
    pub frozen_at: Option<String>,  // populated for baseline files
    pub user_turns: Vec<String>,
    pub events: Vec<TranscriptEvent>,  // user_turn | tool_call | tool_result | assistant_message
    pub deterministic_floor_passed: bool,
    pub usage_summary: UsageSummary,   // rounded token counts, ms duration
}
```

Scenarios where the deterministic floor fails are *not* graded by the
judge — they're reported as a hard-fail in the headline (separate from
paired-comparison wins/losses). Floor failures are always regressions
regardless of how the judge feels about the rest of the transcript.

### Per-scenario axis override

Default axes: `correctness`, `efficiency`, `conciseness`. Scenarios can
override via an optional field in `scenario.toml`:

```toml
name = "stop-guessing-after-failures"
# ...
[scoring]
axes = ["robustness", "efficiency"]  # opt-in override
```

Most scenarios inherit the defaults; the few with specialized lenses
(your existing postmortem-derived ones) declare their own. The judge
prompt is parameterized over the axes list.

### Multi-run

`scenario.toml`'s existing `runs: NonZeroUsize` field becomes load-
bearing. Default 3, per-scenario override allowed. The
`runner.rs:69` bail on `runs > 1` is removed.

For each scenario, the runner produces `Vec<CapturedRun>` of length
`scenario.runs`. Each capture is normalized and written to the results
file independently. Aggregation happens at report time:

- Each of the K new captures is paired-compared against the single
  canonical baseline → K verdicts × A axes per scenario.
- Every (scenario × run × axis) cell produces exactly one verdict
  ∈ {CurrentWins, BaselineWins, Tie}. The total number of verdicts
  for a suite of S scenarios at K runs each with A axes is `S × K × A`.
- Aggregation at any granularity (per-scenario, per-axis, suite-wide)
  is just summing verdicts within the relevant slice. The headline
  metric (next section) operates on the suite-wide slice.

The baseline is a single canonical transcript, not K transcripts. This
treats the baseline as a fixed reference and the new runs as samples of
current behavior. Pairwise across N×N pairs adds aggregator complexity
without obvious signal benefit.

## Judge

### New method on Judge struct

```rust
impl Judge {
    pub async fn compare(
        &self,
        baseline: &NormalizedTranscript,
        current: &NormalizedTranscript,
        axes: &[Axis],
        user_turns: &[String],
    ) -> CompareVerdict { /* ... */ }
}
```

Lives alongside the existing `Judge::evaluate` (single-transcript
absolute judgment). The existing method stays — it's still useful for
scenarios where there's no baseline yet (brand-new scenarios) or for
the deterministic-floor's own LLM-graded assertions.

### Single call, structured multi-axis output

One judge invocation per (scenario-pair × run-index). Returns per-axis
verdicts plus per-axis rationale in a structured response. Schema:

```yaml
correctness:
  verdict: current_wins | baseline_wins | tie
  rationale: "Brief justification"
efficiency:
  verdict: ...
  rationale: ...
conciseness:
  verdict: ...
  rationale: ...
```

Cost shape at default settings (N=3, 10 scenarios, 3 axes):
~30 judge calls per `steve eval` invocation. On a Haiku-class judge
relayed through Fuel-IX, ~$0.15–$0.25 total. Affordable on every PR.

### Halo-effect mitigation in the prompt

A judge asked to score on 3 axes simultaneously can anchor across them
("transcript A is obviously better, so it wins everywhere"). The prompt
explicitly instructs:

1. Per-axis independence: rationale is required *before* verdict on each
   axis, in order; do not consider other axes when justifying one.
2. Tie is a first-class verdict: "if both are roughly equivalent on this
   axis, return tie" is repeated in the prompt.
3. Order randomization: which transcript is "A" and which is "B" in the
   prompt is randomized per call to neutralize position bias.

Per-axis rationale becomes load-bearing for debugging — when a judge
verdict surprises a reviewer, the rationale shows whether the judge
understood the axis correctly.

## Reporting

### Layered output

Default `steve eval report` output:

```
Eval results — current vs baseline (frozen 2026-04-15 at abc1234)

  Headline:        +2.4% net win rate (98% non-regression)
  Hard floor:      10/10 scenarios passed deterministic assertions

  Per axis:
    correctness:   +1.8% net win rate (won 18 / lost 12 / tied 60)
    efficiency:    +4.1% net win rate (won 24 / lost 11 / tied 55)
    conciseness:   +1.3% net win rate (won 14 / lost 10 / tied 66)

  See --verbose for per-scenario breakdown.
```

Three layers:

1. **Headline**: signed delta in win rate vs baseline + non-regression
   rate. The thing CI prints, the thing humans read first.
2. **Per-axis**: signed delta per axis with raw win/loss/tie counts.
   Visible by default — small enough to fit on screen.
3. **Per-scenario**: full grid behind `--verbose`. Used for debugging
   surprising headlines or investigating specific scenarios.

### Two formulas, both load-bearing

Let `W`, `L`, `T` be the suite-wide totals of `CurrentWins`,
`BaselineWins`, and `Tie` verdicts across all `S × K × A` cells.

- **Net win rate** = `(W - L) / (W + L + T)` — signed; ties contribute
  zero. Range `[-1.0, +1.0]`. This is the headline number. `+0.024`
  reads as "current is the clear winner 2.4% more often than baseline,"
  `-0.014` reads as "current is the clear loser 1.4% more often." All
  ties = 0.0, no change. Random 50/50 with no ties = 0.0, no change.
- **Non-regression rate** = `(W + T) / (W + L + T)` — "how often the
  new build wasn't worse." Range `[0.0, 1.0]`. Sits beside the headline
  as a confidence check.

A `60% non-regression` paired with `+2.4% net win rate` is a much weaker
signal than `99% non-regression` with `+2.4% net win rate` — same
headline, very different confidence. Both reported. Per-axis breakdowns
use the same formulas applied to per-axis slices.

### Baseline provenance

Every report's metadata block records the baseline's git ref + freeze
date + judge model identity. This makes reports interpretable when
read three months later. Does not affect the headline number; sits in
the metadata block.

### History file (`eval/history.jsonl`)

A long-lived JSONL file in the repo, one row per recorded report.
Provides the cumulative-improvement signal the design was missing — a
place to ask "show me net win rate per commit for the last month."

Schema per row:

```json
{
  "git_ref": "abc1234",
  "recorded_at": "2026-05-06T14:23:00Z",
  "model": "ollama/qwen3-coder",
  "baseline_git_ref": "main/2026-04-15",
  "judge_model": "fuel-ix/claude-haiku-4-5",
  "headline": { "net_win_rate": 0.024, "non_regression_rate": 0.98 },
  "per_axis": {
    "correctness": { "net_win_rate": 0.018, "won": 18, "lost": 12, "tied": 60 },
    "efficiency": { "net_win_rate": 0.041, "won": 24, "lost": 11, "tied": 55 },
    "conciseness": { "net_win_rate": 0.013, "won": 14, "lost": 10, "tied": 66 }
  },
  "deterministic_floor": { "passed": 10, "total": 10 },
  "results_file": "path/to/results.yaml"
}
```

Write semantics: **append only on explicit flag** —
`steve eval report --record-history`. Bare `steve eval report` is
read-only against the file. This keeps local exploratory runs from
producing git churn; CI-on-main runs with the flag and commits the
appended row, building the canonical history.

Read semantics: any run of `steve eval report --html` reads the file
to render trend charts (see "HTML report" below). External tools
(`duckdb`, `jq`, `pandas`) can ingest it directly without going
through Steve.

### HTML report (`steve eval report --html report.html`)

Self-contained single-file HTML output for human consumption. CLI text
output is the CI gate; HTML is the dashboard you actually look at.

Layout:

1. **Latest run** — headline + per-axis + per-scenario detail table.
   Same content as `--verbose` CLI output but readable.
2. **Trends over time** (read from `eval/history.jsonl`) — line chart
   of net win rate per commit, per-axis overlays, markers for
   baseline-refresh events. Skipped if history.jsonl is empty.
3. **Per-scenario links** — table of scenarios with links into
   `eval/scenarios/<name>/scenario.toml` and the rendered transcript.

Implementation: pure HTML + Chart.js via CDN, no build step. Embeds
all data inline so the file is self-contained — can be attached to
issues, hosted as a GitHub Pages artifact, or just opened locally.

## CLI surface

### Verbs

- `steve eval` — chains run → report against the configured baseline.
  Default common case.
- `steve eval run [--model X] [--out path]` — runs scenarios, writes
  results.yaml. No judging.
- `steve eval report <results.yaml> [--baseline path] [--verbose] [--record-history] [--html path]` —
  runs the judge, prints layered output. `--record-history` appends a
  row to `eval/history.jsonl`. `--html path` writes a self-contained
  HTML report. Exit code reflects regression threshold (configurable;
  default: any negative headline delta is exit 1).
- `steve eval baseline freeze [--scenario X] [--model Y]` — runs scenarios,
  writes baseline files, updates manifest.

### Use case mapping

| Use case | Command |
|----------|---------|
| PR regression check | `steve eval` (uses checked-in baseline) |
| Cross-model compare | `steve eval --model ollama/qwen3-coder` (compares against ollama baseline) |
| Compare to older anchor | `git checkout v0.4.0 -- eval/baselines/ && steve eval` (uses checked-out baseline files); long-term, replaced by named anchor manifest |
| Backtest judge changes | `steve eval report archived-results.yaml` (re-grades old transcripts) |
| Cross-baseline comparison | `steve eval report current.yaml --baseline some-other.yaml` (any two results files) |

### Baseline workflows

Concrete user-facing flows for creating, refreshing, and managing
baselines. All examples below assume the configured model is
`ollama/qwen3-coder` unless specified.

**First-time baseline (fresh checkout, no baselines exist):**

```
$ steve eval baseline freeze
$ git add eval/baselines/
$ git commit -m "Freeze initial baselines for ollama/qwen3-coder"
```

Bare `baseline freeze` runs every scenario under `eval/scenarios/` once
with the configured model, normalizes each transcript, writes one file
per scenario to `eval/baselines/<scenario>/<model_id>.yaml`, and
updates `eval/baselines/manifest.toml`.

**Per-scenario or per-model freeze:**

```
$ steve eval baseline freeze --scenario stop-guessing-after-failures
$ steve eval baseline freeze --model anthropic/claude-haiku-4-5
$ steve eval baseline freeze --scenario X --model Y
```

Filters compose. No flags = all scenarios + configured model.
`--model` runs against models that aren't the configured default
(useful for the cross-model-compare use case — freeze a baseline
once per model you care about).

**Refreshing after an intentional behavior change:**

Workflow: change Steve's system prompt (or whatever), run the
appropriate freeze command, review via `git diff eval/baselines/`,
commit. The committed diff *is* the record of what changed at the
behavioral level; the commit message captures the why.

```
$ steve eval baseline freeze --scenario stop-guessing-after-failures
$ git diff eval/baselines/stop-guessing-after-failures/
# ...review the YAML diff: did the agent's behavior change in the way I expected?
$ git add eval/baselines/stop-guessing-after-failures/
$ git commit -m "Refresh baseline: stop-guessing now stops at attempt 3 (was 5)"
```

No `--force` flag needed; freeze always overwrites. Git's working-copy
state is the safety net — if you don't like the new baseline,
`git checkout eval/baselines/<scenario>/` reverts before you commit.

**Adding a new scenario:**

1. Add `eval/scenarios/new-scenario/scenario.toml` + fixtures.
2. `steve eval baseline freeze --scenario new-scenario` (per model you
   care about).
3. Commit the new scenario *and* baselines together.

The partial-baseline policy (see "No-baseline handling" below) means
the eval suite gracefully skips a scenario that has no baseline for
the configured model. So the order between "add scenario" and "freeze
baseline" doesn't matter strictly — but committing both together
keeps history clean.

**Adding a new model to the tracking set:**

```
$ steve eval baseline freeze --model anthropic/claude-haiku-4-5
$ git add eval/baselines/
$ git commit -m "Add claude-haiku-4-5 baselines"
```

Now `steve eval --model anthropic/claude-haiku-4-5` has something to
compare against. The default model's baselines are unaffected.

### Number of runs during freeze

`baseline freeze` always does **one** run per scenario, regardless of
`scenario.runs` setting. The baseline is one canonical transcript
captured at a moment in time. The variance-vs-baseline asymmetry is
intentional: the baseline is the *fixed reference*; the current side
runs K samples and aggregates K verdicts to reduce noise.

Doing N runs at freeze time and picking "best" or "median" would
require defining "best run," which requires a judge — circular,
since the judge is what we're trying to use the baseline to enable.
If a freeze run lands on an unrepresentative outlier, the user re-runs
and `git diff` exposes the difference before commit. User agency >
clever heuristics.

### No-baseline handling

Two cases, two policies:

**Targeted invocation** — `steve eval --scenario X` against a missing
baseline: **fail loud**, print the exact command to baseline:

```
error: no baseline for scenario 'X' with model 'Y'
       run: steve eval baseline freeze --scenario X --model Y
```

**Whole-suite invocation** — `steve eval` (no scenario filter) where
*some* scenarios have baselines and others don't: skip the missing
ones, compute the headline over the rest, surface the gap explicitly:

```
Eval results — current vs baseline (frozen 2026-04-15 at abc1234)

  Headline:        +2.4% net win rate (98% non-regression)
  Skipped:         2 scenarios (no baseline for model 'ollama/qwen3-coder')
                   - find-symbol-vs-grep
                   - lsp-rename-vs-sed
                   run: steve eval baseline freeze --model ollama/qwen3-coder
```

If *no* scenarios have baselines for the configured model: fail loud,
same shape as targeted-invocation. The "all-missing" case is almost
always a fresh-checkout-without-baselines mistake; the "some-missing"
case is the natural state when adding new scenarios.

No auto-baseline in any case (would enshrine a possibly-bad transcript
as the gold standard).

### Regression threshold for exit code

`steve eval report` exit code:

- `0` — net win rate ≥ threshold
- `1` — net win rate < threshold (regression)
- `2` — eval infrastructure error (config, missing API key, etc.)

Threshold sources, highest precedence first:

1. CLI flag: `--regression-threshold <float>` (e.g., `--regression-threshold -0.02`
   to allow up to 2% negative drift before failing CI).
2. Project config: `eval.regression_threshold` in `.steve.jsonc`.
3. Default: `0.0` (any net negative delta is a regression).

### Judge model selection

Precedence, highest first:

1. CLI flag: `--judge-model <provider/model>`.
2. Per-scenario: `judge_model` field in `scenario.toml`.
3. Error: no fallback. The runner already validates judge config
   loud at startup (`Runner::build` posture); same applies here.

The baseline manifest's `judge_model` field is **informational only**
— it records which judge graded the baseline at freeze time, so a
reviewer can see "this baseline was graded by Haiku, but my current
report is using Sonnet — calibration may differ."

## What stays from Phases 1–5

Most of the existing eval module is keepable. The pivot is at the *result
type layer*, not the *infrastructure layer*. Specifically:

| Component | Status |
|-----------|--------|
| `Scenario` TOML format (`scenario.rs`) | Extended with optional `[scoring]` block |
| `ScenarioWorkspace` + setup/fixtures (`workspace.rs`) | Unchanged |
| `Runner` driver + `run_until_idle` (`runner.rs`) | Multi-run added; bail on `runs > 1` removed |
| `CapturedRun` + event observation (`capture.rs`) | Unchanged; Normalizer reads from it |
| Provider/judge registry isolation | Unchanged |
| Rule-based assertions (`expectations.rs`) | Stays as deterministic floor |
| `Judge::evaluate` single-transcript method | Stays; new `Judge::compare` added beside |
| 10 Phase-5 scenarios | Unchanged inputs; new outputs |

## Phases

Three phases, each with a self-contained testable deliverable. Each lands
as one or more PRs targeted at the existing long-lived `feat/eval-harness`
branch. Final consolidated review of the whole eval epic before merge to
main.

### Phase 6 — Data Foundation (`steve-tk30`)

Scope:

- Schema overhaul: add `PairedScore`, `ScenarioScore`, `Axis`,
  `Verdict`, `NormalizedTranscript` types.
- `Normalizer` helper: `CapturedRun` → `NormalizedTranscript` (strips
  noise, canonicalizes paths).
- Multi-run: honor `Scenario.runs`, runner produces `Vec<CapturedRun>`,
  remove the `runs > 1` bail at `runner.rs:69`.
- Baseline storage: read/write helpers for
  `eval/baselines/<scenario>/<model>.yaml` via serde-saphyr;
  `manifest.toml` reader/writer.
- `steve eval baseline freeze` subcommand.
- `steve eval run` subcommand (writes results.yaml; no judge).

Ships when:

- A user can `steve eval baseline freeze --scenario _smoke --model X`
  and inspect the YAML by hand.
- A user can `steve eval run --scenario _smoke --model X` and get a
  multi-run results.yaml.
- All Phase-5 scenarios baseline successfully against a configured
  default model.
- No reporting yet; `steve eval` without subcommand still does the
  Phase-5 single-run pretty-JSON output (preserved for non-disruption).

### Phase 7 — Paired-Comparison Judge (`steve-xa5u`)

Scope:

- `Judge::compare` method with structured multi-axis output.
- Halo-mitigation prompt design (per-axis-rationale-before-verdict,
  tie as first-class, A/B order randomization).
- Per-scenario axis override parsing in `scenario.toml`'s `[scoring]`
  block.
- Unit tests on canned transcript pairs covering: clear win on each
  axis, mixed verdicts (won correctness, lost efficiency), all-tie,
  baseline-wins.

Ships when:

- `Judge::compare` returns plausible verdicts on hand-crafted pairs.
- The prompt is robust enough that swapping A/B in the same call
  produces inverted but otherwise consistent verdicts.

### Phase 8 — Reporting + CLI Split (`steve-u896`)

Scope:

- `steve eval report <results> [--baseline path]` subcommand.
- Layered text output (headline + per-axis + verbose per-scenario).
- Net win rate + non-regression rate formulas.
- Baseline provenance metadata in every report.
- `steve eval history.jsonl` append on `--record-history` flag.
- `steve eval report --html path` writes a self-contained HTML
  report covering the latest run + trends-over-time chart from
  `eval/history.jsonl`.
- `steve eval` (no subcommand) reshaped to chain run → report against
  the configured baseline. Phase-5's single-run pretty-JSON path is
  retired.
- Exit code semantics: regression threshold configurable; default exit 1
  on negative headline delta.
- No-baseline error path with copy-pasteable command suggestion.
- Partial-baseline graceful degradation (skip-with-warning, headline
  over the rest).

Ships when:

- `steve eval` end-to-end produces the layered text output against a
  real Phase-5 scenario.
- `steve eval report --html` produces a viewable single-file HTML
  output that includes the latest-run breakdown and (if history.jsonl
  is non-empty) a trends chart.
- Backtest works: re-running `steve eval report` against a prior
  results.yaml reproduces the same headline (modulo judge variance).
- CI can be wired up to gate on the exit code and to commit
  history.jsonl appends on main.

## Issue housekeeping

- **`steve-53nw`** (original Phase 6: JSONL output + summary table +
  steve eval compare) — **closed 2026-05-06 as superseded.** New scope
  is fundamentally different.
- **New bd issues created 2026-05-06:**
  - `steve-tk30` — Phase 6 (data foundation)
  - `steve-xa5u` — Phase 7 (paired-comparison judge)
  - `steve-u896` — Phase 8 (reporting + CLI split)
  - Dependencies wired: 7→6, 8→7, epic (`steve-ffdq`) blocks on all three.
- **`steve-mxpe`** (scenario-from-debug generator, formerly Phase 7) —
  unchanged; deferred. Renumbered to Phase 9 in human-facing labels;
  the bd issue itself doesn't need to change.
- **`steve-paeu`** (multi-run majority-pass) — **closed 2026-05-06 as
  folded** into `steve-tk30` (semantics changed completely; no longer
  "majority pass" but "K samples × A axes paired-compared").
- **`steve-c0uk`** (`MaxToolCalls` count-only) — independent of pivot;
  stays open.
- **`steve-ulek`** (USD cost in output) — independent of pivot; stays
  open. Worth doing alongside Phase 8 reporting since the report block
  is the natural place for cost output.
- **`steve-f3v8`** (walking test pairs scenarios with VALIDATION.md
  sections) — independent; stays open.
- **`steve-ux92`** (`#[should_panic]` coverage for walking test) —
  independent; stays open.

## Open questions / future work

These were considered and deliberately deferred. Filed bd issues are
linked; the rest are noted here for "if you find yourself wanting X,
this is the cheap path."

1. **Anchor-baseline manifest** (`steve-6hes`, P3). Named slots like
   `[baselines.v0_4_0]` in the manifest, with `--baseline-tag v0.4.0`
   CLI flag. Build when there's an actual second anchor to track.
2. **Judge-verdict caching** (`steve-2a11`, P3). Keyed on
   (transcript-hash, baseline-hash, judge-model, prompt-version).
   Useful when re-reporting becomes common.
3. **Elo / Glicko rating** for many-version comparison. Not filed —
   speculative; file when there are >5 historical baselines worth
   comparing.
4. **Adaptive multi-run** (run more samples until convergence). Not
   filed — speculative; file only if fixed-N variance is observed to
   be a real problem.
5. **Transcript pruning** for repo size. Not filed — speculative; file
   only if baseline files start exceeding a few hundred KB each.
6. **MCP tool calls in `CapturedRun`** (existing `steve-ap0q`). Already
   filed; relevant if MCP-using scenarios get authored.
7. **GitHub Pages auto-publish of HTML report** (`steve-fl4c`, P3).
   CI uploads the HTML artifact and deploys to a Pages site for a
   public dashboard URL. File-then-defer pattern; build when there's
   a reason for public visibility (resume / portfolio link).
8. **HTML report polish iterations.** Not filed — speculative; file
   specific feature requests as they arise from actual use rather
   than speculating now.

## Considered alternatives

External tools that were evaluated and declined for this design.
Preserved here so future readers can see the reasoning rather than
re-litigating from scratch.

### MLflow

**What it is:** Open-source experiment tracking platform from Databricks.
Designed for ML training pipelines: hyperparameter sweeps, model
artifacts in GB, experiment / run / metric / artifact data model. Web
UI for browsing and comparing runs. Self-hostable; also offered as a
managed service.

**Why considered:** Already in use at the project author's workplace
for benchmark output tracking. Familiar tooling; lowers the cognitive
overhead of adopting *something* for trend tracking.

**Why declined:**

- **Wrong size.** Designed for ML training scale (hundreds of params,
  GB-scale artifacts, model registries). Steve's eval is much narrower:
  a few axes, plain-text transcripts, a handful of models.
- **Data-model mismatch.** Mapping "scenario × model × runs × axes" into
  MLflow's "experiment / run / params / metrics" abstraction is awkward
  in both directions. The work to make the integration feel native is
  larger than just writing the JSONL append.
- **Operational cost.** Either run an MLflow server (process to
  manage, DB to back it) or use the file backend (which is awkward
  and feels like "I want plain files but with extra steps").
- **Cross-language friction.** Steve is Rust; MLflow is Python-native.
  `mlflow-rs` exists but is small and incomplete. The API surface
  needed for our use case is small enough that we don't gain anything
  from the SDK.
- **User signal.** The project author describes the work setup as
  "we use it but it's grown a lot" — when an existing user describes
  a tool that way, it's usually a flag that scope has outgrown the
  use case.

**What we'd do if MLflow were already mandatory in this project:** ship
the same design, write a thin adapter that pushes the JSONL rows into
MLflow as runs/metrics. Keep the JSONL as the source of truth.

### Langfuse

**What it is:** Open-source LLM observability platform — agent traces,
prompt management, eval datasets, multi-axis scorers. Designed for
production LLM apps. Free hobby tier on the cloud-hosted version
(50k events/month at last check); also self-hostable (Postgres +
ClickHouse + Redis).

**Why considered:** Closer domain match than MLflow (designed for LLM
apps specifically); free hobby tier covers Steve's eval volume; OSS so
no vendor risk. The project author uses it at work — additional
career-relevant familiarity is genuine extra value. Steve is
open-source so privacy of cloud-hosted traces isn't a blocker.

**Why declined for this pivot specifically:**

- **Wrong consumer.** Langfuse's strengths are real-time multi-user
  prod observability — latency monitoring, A/B testing across many
  concurrent users, dataset management at team scale. Steve's eval
  use case is "single user runs 10 scenarios when making a code
  change." Most of Langfuse's surface area would go unused.
- **Network dependency in CI.** Eval runs would do HTTP calls to
  Langfuse during the report stage. Slow, flaky in offline dev,
  another moving part to fail.
- **Data lives elsewhere.** Trends-over-time queries go through
  Langfuse's UI/API, not through grep on a checked-in file. That's
  actively worse for a project that values plain-text-in-git.
- **Self-hosted is real work.** Postgres + ClickHouse + Redis stack
  for a hobby project is too much. Project author explicitly opted out.

**Where Langfuse might still be valuable, separately:** as a runtime
observability target for Steve itself (live agent traces during dev
sessions, optional opt-in). That's a different project from the eval
harness pivot. File as a future exploration if the runtime-debugging
itch gets stronger.

### OTEL + Jaeger via the `tracing` crate

**What it is:** OpenTelemetry distributed-tracing standard, viewable
in Jaeger or similar. Steve already uses the Rust `tracing` crate;
adding OTEL export is an existing-toolkit-extends move.

**Why considered:** Project author noticed possible overlap between
eval transcripts and runtime debugging traces — both are "things that
happened during an agent run." If the same infra served both, less
duplication.

**Why declined:**

- **Wrong shape.** Spans are *timing intervals* — they answer "where
  did the time go." Eval transcripts are *behavioral records* — they
  answer "what did the agent do." Forcing one schema to serve both
  loses information at both ends.
- **Wrong storage model.** Jaeger doesn't keep data around long-term
  by default. Cumulative-improvement tracking ("net win rate over
  the last 6 months") needs durable storage; spans are ephemeral
  by design.
- **Wrong query model.** Span-based tooling answers "show me the
  slow tool call" cleanly; it doesn't naturally answer "show me how
  agent behavior changed between commits."

**Where OTEL/tracing might still be valuable, separately:** for
runtime debugging in dev sessions. The `CapturedRun` event-stream
architecture is already similar in spirit; could in principle be
exposed as OTEL spans for a "view this run in Jaeger" debugging
workflow. Unrelated to the eval pivot. File as a follow-up if the
debugging UX gets painful.

### What stays plain-file-in-git

The defensible case for *not* adopting any of these tools:

- **All three solve organizational problems Steve doesn't have.**
  MLflow exists because ML teams can't put model weights in git.
  Langfuse exists because production LLM apps can't put trace data
  in git. OTEL exists because microservice fleets can't centralize
  logs without a standard. Steve has none of those constraints.
- **Plain text in git preserves all downstream choices.** Any of
  these tools can ingest YAML or JSONL trivially if needed later —
  the reverse isn't true. Choosing the simplest representation first
  is the option-preserving move.
- **The "do I need a dashboard" felt-need test:** ship Phase 8 with
  the static HTML report, run the suite for a month, and only adopt
  external tooling if the local setup actively breaks down. If it
  doesn't, you didn't need it.

## Design decisions log

For future reference (and to prevent re-litigating in implementation):

| Decision | Choice | Date |
|----------|--------|------|
| Default axes | correctness, efficiency, conciseness | 2026-05-06 |
| Per-scenario axis override | Optional `[scoring]` block in `scenario.toml` | 2026-05-06 |
| Baseline storage location | `eval/baselines/` in repo, plain text | 2026-05-06 |
| Baseline file format | YAML via serde-saphyr | 2026-05-06 |
| Compression | None (plain text for diffability) | 2026-05-06 |
| Multi-run default | N=3, per-scenario override | 2026-05-06 |
| Baseline shape | Single canonical transcript, not N | 2026-05-06 |
| Judge call structure | One call per pair, structured multi-axis output | 2026-05-06 |
| Headline metric | Layered: net win rate + per-axis + verbose | 2026-05-06 |
| Headline formulas | Net win rate `(W-L)/(W+L+T)` + non-regression rate `(W+T)/total` | 2026-05-06 |
| Run/report coupling | Decoupled — same schema for baselines and run-outputs | 2026-05-06 |
| No-baseline (targeted) | Fail loud with copy-pasteable freeze command | 2026-05-06 |
| No-baseline (whole-suite, partial) | Skip-with-warning, headline over the rest | 2026-05-06 |
| Pass/fail assertions | Kept as deterministic floor; not headline | 2026-05-06 |
| Regression threshold default | `0.0` (any net negative delta = regression); CLI/config override | 2026-05-06 |
| Judge model precedence | CLI flag > `scenario.toml` field > error | 2026-05-06 |
| Custom axes | Deferred — fixed enum for v1, add variants when needed | 2026-05-06 |
| History tracking | `eval/history.jsonl` in repo, append on `--record-history` flag only | 2026-05-06 |
| HTML report | Self-contained single-file via `--html path`, Chart.js for trends | 2026-05-06 |
| Freeze run count | Always N=1, regardless of `scenario.runs` setting | 2026-05-06 |
| Freeze overwrite policy | Always overwrites; `git diff` is the safety net (no `--force` flag) | 2026-05-06 |
| External tracking tools | None (MLflow/Langfuse/OTEL considered, declined — see Considered alternatives) | 2026-05-06 |
| Phase split | Three phases: data → judge → report (HTML + history fold into Phase 8) | 2026-05-06 |
