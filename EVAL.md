# Eval Harness

Steve ships with a **paired-comparison eval harness** for regression-testing
agent behavior across model changes, prompt changes, and code changes. The
core loop:

1. **Freeze** a baseline transcript for each scenario at a known-good point.
2. **Run** the agent again against the same scenarios, capturing K samples per
   scenario.
3. **Judge** each new transcript against its baseline using an LLM judge that
   picks winners on per-axis criteria (correctness, efficiency, etc).
4. **Report** a layered headline with exit code 0 (pass) / 1 (regression) / 2
   (no data or infra error).

The harness is a CLI: `steve eval` and its subcommands. This guide covers the
workflows; for the design rationale see
[`docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md`](./docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md).

## Prerequisites

Before running the examples below:

1. **Install steve**: `cargo install --path .` from the repo root
   drops the binary into `~/.cargo/bin/` (which is usually on
   `$PATH`). If you'd rather not install, build with
   `cargo build --release` and substitute `cargo run --release -- `
   for `steve ` in the examples below.
2. **Configure the providers your agent and judge models live behind**
   in `~/.config/steve/config.jsonc` (or `.steve.jsonc` in the project
   root). You need one **agent model** (the model under test) and one
   **judge model** (the LLM that grades comparisons); they can be
   different providers or the same provider with two model IDs — or
   even the same model in both roles, as the CI snippet later in this
   doc demonstrates. See the [Quick Start in
   README.md](./README.md#quick-start) for the config schema. The
   examples below use `ollama/gemma4` (agent) and
   `ollama/qwen3-coder` (judge); swap in whatever you have
   configured.
3. **The `_smoke` scenario ships with the repo** at
   `eval/scenarios/_smoke/`. Use it as your first end-to-end test —
   you don't need to author a scenario to run the Quick Start.

## Quick start

```bash
# 1. Freeze a baseline for the _smoke scenario.
steve eval baseline freeze --scenario _smoke --model ollama/gemma4

# 2. Run + report against that baseline; needs a judge model.
steve eval --scenario _smoke \
  --model ollama/gemma4 \
  --judge-model ollama/qwen3-coder

# Exit code: 0 = pass, 1 = regression, 2 = no data / infra error.
```

`steve eval --help` and `steve eval baseline freeze --help` show every flag.

## Concepts

### Paired comparison

The judge compares **two transcripts side by side** and picks a winner per
axis — it doesn't grade either in isolation. This is more reliable than
absolute scoring: LLM judges anchor on the comparison, not on a shifting
internal scale.

- **Baseline** — one frozen transcript per (scenario, provider, model), stored
  as YAML on disk. Single sample (K=1) because the diff itself is the audit
  trail; see *Refreshing after an intentional change*.
- **Current** — K fresh samples (default K=3, from `scenario.runs`) captured
  when you run `steve eval`. K > 1 smooths agent variance so the verdict is
  robust.
- **Verdict** per axis: `CurrentWins`, `BaselineWins`, or `Tie`.

### Axes

Scoring dimensions. Default set: **Correctness**, **Efficiency**,
**Conciseness**. Available but not in the default: **Robustness**,
**Truthfulness**. A scenario can override with a `[scoring]` block (see
*Configuration reference*).

| Axis | Question the judge asks |
|---|---|
| Correctness | Did the agent produce the right outcome for the user's task? |
| Efficiency | Did the agent achieve the outcome with fewer/better tool calls? |
| Conciseness | Were the assistant messages succinct and on-point? |
| Robustness | Did the agent handle errors well, or spin / give up / make things worse? |
| Truthfulness | Did the agent ground claims in actual tool output, no fabrication? |

### Headline metrics

Across all (scenario × run × axis) cells:

- **Net win rate** = `(W − L) / (W + L + T)`, range `[−1.0, +1.0]`. Positive
  means the current code wins on average. This is what `--regression-threshold`
  compares against.
- **Non-regression rate** = `(W + T) / (W + L + T)`, range `[0.0, 1.0]`. The
  fraction of cells that aren't a regression (wins + ties).

Both return their "no change" value (0.0 / 1.0) when no cells were graded.
That's why the CLI has a distinct exit code 2 — a meaningless 0.0 must not
be read as Pass by CI.

### Deterministic floor

Separate from paired comparison: every run also goes through **rule-based
assertions** declared in `scenario.toml` (e.g., `tool_called`,
`file_contains`). These run without the judge. The floor's pass/fail per
run is recorded into the baseline YAML and (when `--record-history` is
set) into `eval/history.jsonl`'s `deterministic_floor` field for
downstream analysis. The text report doesn't surface a floor counter
today, and the HTML report's trend chart plots only
`headline.net_win_rate` — the floor data is in the JSONL for ad-hoc
inspection but isn't visualized in v1.

The floor catches structural facts the judge would miss (the right tool
was called, the right file was modified). Both layers ship together —
neither replaces the other.

### Scenario outcomes

Each scenario in a report lands in one of three states:

- **Graded** — at least one run produced verdicts; the scenario's
  successful runs are counted in the headline. Individual runs that
  double-failed at the judge step are tallied as `errored_runs` and
  excluded from the per-axis counts.
- **Skipped** — the scenario contributes no verdicts to the aggregate
  totals. Reasons: no baseline for this (scenario, model) pair,
  `user_turns` drifted from the baseline, `scenario.toml` is
  missing/malformed under the scenarios root, or every run errored
  (the judge did run but double-failed for each one).
- **Errored** (per-run) — a single run failed twice in a row at the
  judge step. The run is excluded from the tally; the scenario as a
  whole stays Graded if any of its other runs succeeded.

## Workflows

### Setting up a scenario

> If you're just trying the harness for the first time, skip to
> *Freezing your first baseline* below — the repo ships several
> scenarios under `eval/scenarios/` (including `_smoke`,
> `find-symbol-vs-grep`, `lsp-rename-vs-sed`, and others). Come back
> here when you want to write your own.

A scenario lives in `eval/scenarios/<name>/scenario.toml` with optional
fixture files alongside.

Minimal example (from `eval/scenarios/_smoke/scenario.toml`):

```toml
name = "_smoke"
description = "Phase 2 smoke: agent should read the lone file and report its contents."
user_turns = [
  "There's one file in this directory. Read it and tell me what it says.",
]

[setup]
copy_fixtures = ["greeting.txt"]

[[expectations]]
kind = "tool_called"
tool = "read"

[[expectations]]
kind = "final_message_contains"
substring = "hello"
case_insensitive = true
```

At runtime:

- The scenario directory is copied to a temp workspace; `setup.copy_fixtures`
  lists which files are needed.
- The agent is given the `user_turns` in FIFO order — first entry is the
  initial prompt, subsequent entries become follow-ups after each completed
  assistant response.
- Each run produces one transcript; `runs = N` (default 3) samples N
  transcripts per scenario.

See *Configuration reference* below for the full schema.

### Freezing your first baseline

```bash
steve eval baseline freeze --scenario _smoke --model ollama/gemma4
```

Output:

```
running scenario _smoke (1/1)... done in 13.0s
froze _smoke -> eval/baselines/_smoke/ollama/gemma4.yaml
updated manifest: eval/baselines/manifest.toml
```

Two things hit disk:

1. **The baseline YAML**: `eval/baselines/<scenario>/<provider>/<model_id>.yaml`
2. **The manifest**: `eval/baselines/manifest.toml` (one entry per frozen
   (scenario, model) pair)

Both are **committed to git** — `.gitignore` explicitly notes "baselines are
committed". Results files (in `eval/results/`) are gitignored.

Freeze is **K=1** regardless of `scenario.runs` because a baseline is a fixed
reference, not a multi-sample artifact. The diff itself records what changed
across re-freezes (see below).

To freeze every scenario in one go, omit `--scenario`:

```bash
steve eval baseline freeze --model ollama/gemma4
```

The operation is run-then-write: every scenario runs to completion in
memory before any disk write, so a **scenario-run failure** (judge
timeout, agent crash) leaves the baselines tree untouched. The
**write phase** can still partially update — if a `baseline.write_to_path`
fails midway through the commit loop (filesystem error, permission
flip), earlier baselines in the same run are already on disk. The
error message names the count of already-written baselines and tells
you to re-run freeze to restore a consistent state.

### Running and comparing

The chained form runs all K samples and then compares against the baseline
in one command:

```bash
steve eval --scenario _smoke \
  --model ollama/gemma4 \
  --judge-model ollama/qwen3-coder \
  --regression-threshold 0.0
```

Sample output:

```
running scenario _smoke (1/1) [axes: correctness, efficiency, conciseness]...
  run 1/3... done in 11.9s
  run 2/3... done in 9.2s
  run 3/3... done in 9.3s
wrote results to /Users/you/proj/eval/results/chained-_smoke-20260513-060721.yaml

Eval results — current (ollama/gemma4 at 318516b) vs baseline
  baseline frozen 2026-05-13T05:46:42Z at 27a328f (1 scenarios)

  Headline:        +33.3% net win rate (88.9% non-regression)

  Per axis:
    correctness:   +66.7% net win rate (won 2 / lost 0 / tied 1)
    efficiency:    +33.3% net win rate (won 1 / lost 0 / tied 2)
    conciseness:   +0.0% net win rate (won 0 / lost 0 / tied 3)

  See --verbose for per-scenario breakdown.
```

Pass `--verbose` for per-scenario detail (each scenario's per-axis breakdown
+ baseline provenance). Pass `--html /tmp/report.html` to also write a
self-contained HTML dashboard. Add `--record-history` to append one row to
`eval/history.jsonl` — the HTML report's trend chart reads that file.

Three command shapes for different needs:

| Command | What it does |
|---|---|
| `steve eval [flags]` | Chained: sample K transcripts + compare to baseline. Most common. |
| `steve eval run --model ... --out path.yaml` | Sample only; no judging. Useful when you want to judge later with multiple judge models. |
| `steve eval report results.yaml --judge-model ...` | Judge an existing results file. Pairs with `run` for back-testing. |

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Pass — net win rate ≥ threshold |
| 1 | Regression — net win rate < threshold |
| 2 | Infra error OR no scenarios graded (all skipped/errored) |

### Refreshing baselines after an intentional change

Rather than tagging baselines with semantic versions or maintaining a
separate behavior-changelog, this harness uses the **YAML diff itself
as the audit record**: `git log eval/baselines/` is your behavior
changelog and `git blame` answers "when did this behavior change?".

**When to re-freeze**: after a change you've decided to keep — not
"after a change you suspect might be better." The baseline is the
*desired* behavior, not the best possible behavior. So:

- You changed the system prompt and verified (via `steve eval`) the
  new transcripts look right → freeze. The judge's per-axis verdicts
  may show losses on some axes; that's fine if the trade-off is
  intentional (e.g., dropped a verbose preamble; "conciseness" wins,
  "completeness" loses — both intentional).
- You're not sure if your change is an improvement → run `steve eval`
  WITHOUT freezing first. Read the layered headline. If the verdict
  is unfavorable and you don't want the new behavior, revert the
  source change; don't freeze. If you do want the new behavior,
  freeze.
- You suspect a regression you didn't intend → DON'T freeze. The
  whole point of the harness is to catch this and surface it as
  exit code 1.

The workflow once you've decided:

```bash
# 1. Make your agent change in source.
$EDITOR src/app/constants.rs

# 2. Re-freeze the affected scenarios.
steve eval baseline freeze --scenario _smoke --model ollama/gemma4

# 3. The diff IS the audit trail.
git diff eval/baselines/_smoke/ollama/gemma4.yaml

# 4. Commit with a message explaining what behavior changed.
git add eval/baselines/_smoke/ollama/gemma4.yaml eval/baselines/manifest.toml
git commit -m "eval: re-freeze _smoke after dropping verbose preamble from system prompt"
```

Because baselines are plain YAML and committed to git, the history of every
behavior change is captured in `git log eval/baselines/`. `git blame` shows
when each behavior froze and why. No separate "behavior changelog" file
needed.

### Reading a baseline YAML by hand

A baseline is a plain YAML file you can read in any editor. Example
(`eval/baselines/_smoke/ollama/gemma4.yaml`):

```yaml
scenario: _smoke
model: ollama/gemma4
git_ref: 27a328f
frozen_at: 2026-05-13T05:46:42Z
user_turns:
- There's one file in this directory. Read it and tell me what it says.
transcript:
  events:
  - kind: tool_call
    tool_name: list
    arguments:
      path: '.'
  - kind: tool_result
    tool_name: list
    output: greeting.txt (20B)
    is_error: false
  - kind: tool_call
    tool_name: read
    arguments:
      path: greeting.txt
  - kind: tool_result
    tool_name: read
    output: "   1 | hello, eval harness\n"
    is_error: false
  - kind: assistant_message
    text: "The file `greeting.txt` says:\n```\nhello, eval harness\n```"
  deterministic_floor_passed: true
  usage_summary:
    prompt_tokens: 15818
    completion_tokens: 114
    total_tokens: 15932
    duration_ms: 12961
```

Field by field:

| Field | What it is |
|---|---|
| `scenario` | Scenario name (must match the directory under `eval/scenarios/`) |
| `model` | The model whose behavior is captured, in `provider/model_id` form |
| `git_ref` | Short git hash of the workspace when freeze ran |
| `frozen_at` | ISO 8601 UTC timestamp of the freeze |
| `user_turns` | The prompts the agent saw (copied from `scenario.toml` at freeze time) |
| `transcript.events` | The agent's behavior, normalized: tool calls, tool results, assistant messages, in order |
| `transcript.deterministic_floor_passed` | Whether the rule-based assertions all passed |
| `transcript.usage_summary` | Token counts + wall-clock duration |

**What's normalized away** to make baselines stable across runs:
- Timestamps within events (order is the array index)
- Workspace tempdir prefix stripped (`/tmp/<uuid>/greeting.txt` →
  `/greeting.txt`; the normalizer drops the prefix substring without
  re-adding the slash, so the leading `/` from the original absolute
  path is preserved)
- Tool-call UUIDs
- Empty assistant messages

This means diffs across re-freezes show **only** the meaningful behavior
changes — not noise from temp paths or timing.

The manifest at `eval/baselines/manifest.toml` is a flat index of every
frozen (scenario, model) pair with its git_ref and frozen_at:

```toml
[[baseline]]
scenario = "_smoke"
model = "ollama/gemma4"
git_ref = "27a328f"
frozen_at = "2026-05-13T05:46:42Z"
```

The manifest is **the inventory of what baselines exist** — a cross-
baseline index you can scan without opening every YAML. `freeze`
reads and writes it; `report` doesn't consume it (the headline's
"frozen at X" provenance is read directly from each baseline YAML's
`git_ref` / `frozen_at` fields).

### Back-testing judge changes

The `run` and `report` subcommands let you sample once and judge multiple
times — useful when picking between candidate judges (e.g., "is the cheaper
judge good enough?").

```bash
# 1. Sample once with no judging.
steve eval run --scenario _smoke --model ollama/gemma4 --out /tmp/results.yaml

# 2. Judge with candidate A (cheap, local).
steve eval report /tmp/results.yaml --judge-model ollama/qwen3-coder

# 3. Judge the SAME results with candidate B (trusted reference).
steve eval report /tmp/results.yaml --judge-model anthropic/claude-haiku-4-5

# Compare verdicts side by side.
```

The agent transcripts are identical across both `report` runs — only the
judge's opinion differs. Use this to validate that a faster/cheaper judge
agrees with your trusted reference judge on a known set of regressions.

### CI integration

The harness's primary CI surface is the **exit code**: 0 = pass, 1 =
regression, 2 = no data or infra error. Wire it up like any other check.

Example GitHub Actions step (matches the shape of
`.github/workflows/ci.yml`). The snippet uses `anthropic/` model
identifiers as one concrete choice — substitute whatever providers your
own `config.jsonc` defines:

```yaml
  eval:
    name: Eval
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build steve
        run: cargo build --release
      - name: Run eval
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
        run: |
          ./target/release/steve eval \
            --model anthropic/claude-haiku-4-5 \
            --judge-model anthropic/claude-haiku-4-5 \
            --regression-threshold 0.0 \
            --record-history \
            --html eval-report.html
      - name: Upload report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: eval-report
          path: |
            eval-report.html
            eval/history.jsonl
```

What each flag contributes:

| Flag | Role |
|---|---|
| `--regression-threshold 0.0` | Step fails (exit 1) if net win rate < 0.0 |
| `--record-history` | Append a row to `eval/history.jsonl` for trend tracking |
| `--html eval-report.html` | Self-contained dashboard archived as a CI artifact |

A few gotchas the snippet papers over and a real CI must handle:

- **Provider config** — `.steve.jsonc` is gitignored, so CI needs its
  own `~/.config/steve/config.jsonc` materialized at runtime (a
  separate step that writes the file from secrets, or a checked-in
  CI-only config under a different path).
- **Env var name** — provider API-key env vars are whatever
  `api_key_env` is set to per provider in your config; `ANTHROPIC_API_KEY`
  in the snippet is an example assuming the obvious anthropic mapping.
- **Judge cost** — judge calls happen on every CI run; pick a small
  cheap judge or run eval on a schedule rather than every PR.
- **Baselines must be in the checkout** — `actions/checkout@v6` covers
  this since baselines live under `eval/baselines/` in the repo.

The harness's exit-code contract is tool-agnostic; the same logic
works on GitLab CI, CircleCI, Jenkins, or a local `make` target. Just
check `$?` after `steve eval` and act on 0/1/2.

### Cross-machine coordination

The directory layout supports multiple developers freezing different
provider/model combinations without conflict:

```
eval/baselines/
  manifest.toml
  _smoke/
    ollama/gemma4.yaml        ← Alice froze this
    anthropic/claude-haiku-4-5.yaml ← Bob froze this
    openai/gpt-4o.yaml         ← CI froze this
```

Each (scenario, provider, model_id) has its own file. `report` locates
the baseline by computing the path from `(baselines_dir, scenario,
model)` directly — it doesn't consult `manifest.toml`. The manifest is
the human-readable index of what's been frozen (see *Reading a
baseline YAML by hand* above).

When pulling a colleague's PR that re-freezes a baseline, you get their
behavior change as a YAML diff. You can re-run `steve eval` locally to
verify the diff is consistent with your model's behavior.

If you've frozen a baseline that no one else has (e.g., a local-only
model), you can either:
- Commit it (others now have it as reference data; helpful for
  reproducibility), or
- Delete the YAML file locally **and** remove its `[[baseline]]`
  entry from `eval/baselines/manifest.toml`. `report` won't actually
  fail on a dangling manifest entry — it locates baselines by file
  path and Skips with a "no baseline" diagnostic regardless — but
  the manifest is the inventory of what baselines exist, so a stale
  entry mis-documents your local state.

## Configuration reference

### `.steve.eval.jsonc` (project-level)

Optional config at the project root. JSONC format (JSON with comments). All
fields optional; missing fields fall back to defaults.

```jsonc
{
  // Net win rate threshold for the exit code; below this is exit 1.
  // CLI --regression-threshold overrides this.
  "regression_threshold": 0.0,

  // Judge model in provider/model_id format. Falls back to this when
  // --judge-model isn't passed and the scenario.toml doesn't declare one.
  "default_judge_model": "ollama/qwen3-coder",

  // Baselines directory. Relative paths anchored to the project root.
  // Defaults to "eval/baselines".
  "baselines_dir": "eval/baselines"
}
```

A global counterpart at `~/.config/steve/eval.jsonc` is also supported;
project values override global field-by-field.

This file is gitignored (per-developer config).

### `scenario.toml` schema

Required fields:

| Field | Type | Notes |
|---|---|---|
| `name` | string | Must match the scenario directory name |
| `description` | string | Human-readable summary |
| `user_turns` | string[] | FIFO; first is the initial prompt |
| `expectations` | table[] | At least one — see kinds below |

Optional fields:

| Field | Type | Default |
|---|---|---|
| `runs` | non-zero int | 3 |
| `judge_model` | string | none |
| `[setup].copy_fixtures` | string[] | `[]` |
| `[setup].shell` | string[] | `[]` (commands to run in tempdir after fixtures copied) |
| `[scoring].axes` | string[] | `["correctness", "efficiency", "conciseness"]` |

### Expectation kinds

Each `[[expectations]]` block sets `kind = "..."` (snake_case). The
evaluator parses all kinds at scenario load time; unknown kinds fail
loudly.

| Kind | Fields | What it asserts |
|---|---|---|
| `tool_called` | `tool` | The named tool was called at least once |
| `tool_not_called` | `tool` | The named tool was never called |
| `requires_prior_read` | `tool`, `must_read_one_of` | A read-class call against one of the paths preceded `tool` |
| `file_unchanged` | `path` | The file was not modified post-run |
| `file_contains` | `path`, `substring`, `case_insensitive` | The post-run file content contains the substring |
| `final_message_contains` | `substring`, `case_insensitive` | The last assistant message contains the substring |
| `final_message_not_contains` | `substring`, `case_insensitive` | The last assistant message does NOT contain the substring |
| `max_repeat_attempts` | `tool`, `max` | No (tool, args) pair was called more than `max` times |
| `judge` | `pass_when`, `fail_when`, `judge_model` | LLM-as-judge per-scenario expectation. **Not wired into the current `run`/`freeze`/`report` flow** — scenario.toml parses these but `apply_judges()` isn't called from any production path today, so they evaluate as Skipped (counted as passing in the deterministic floor). Use paired-comparison via `report` for LLM judging today. Tracked: `steve-k9hu`. |

For the canonical Rust definitions see `src/eval/scenario.rs` (`Expectation`
enum).

### Precedence chains

Judge model: **CLI `--judge-model` > scenario.judge_model > `default_judge_model` from config > error**.

Regression threshold: **CLI `--regression-threshold` > `regression_threshold` from config > 0.0**.

Baselines directory: **CLI `--baselines-dir` (report only) > `baselines_dir` from config > `eval/baselines/`**.

## Output reference

### Text report

The `Eval results` block is layered:

```
Eval results — current (model at ref) vs baseline
  <baseline provenance: frozen-at + git_ref + scenario count>

  Headline:        +33.3% net win rate (88.9% non-regression)
  Skipped:         N scenarios            <-- only present when N > 0
                   - <name>: <reason>

  Per axis:
    correctness:   +66.7% net win rate (won 2 / lost 0 / tied 1)
    efficiency:    +33.3% net win rate (won 1 / lost 0 / tied 2)
    conciseness:   +0.0% net win rate (won 0 / lost 0 / tied 3)

  See --verbose for per-scenario breakdown.
```

- **Metadata line**: model + current git_ref, then a baseline provenance
  line (`frozen <ts> at <ref>`, or `varied refs — see --verbose` when
  scenarios pin different baseline refs).
- **Headline**: suite-wide net win rate + non-regression rate.
- **Skipped**: present only when at least one scenario was skipped;
  lists each skipped scenario with its reason.
- **Per axis**: per-axis tally for every axis in the resolved axis set.
- **`--verbose`**: adds a `Per scenario:` section after the per-axis
  block, showing each scenario's outcome (Graded with per-axis
  breakdown + baseline provenance, or Skipped with reason).

### HTML report

`--html PATH` writes a single self-contained HTML file with the same data
plus a trend chart from `history.jsonl` when available. Open it in a
browser — no server, no external dependencies. Chart.js is inlined; the
file works offline.

Useful CI pattern: upload the HTML as a build artifact so reviewers can
inspect a run from the browser without re-running locally.

### `history.jsonl`

When `--record-history` is set, one JSON object per line is appended to
`eval/history.jsonl`:

```json
{"git_ref":"abc1234","recorded_at":"...","model":"...","baseline_git_ref":"...","judge_model":"...","headline":{"net_win_rate":0.15,"non_regression_rate":0.8},"per_axis":{"correctness":{"net_win_rate":0.2,"won":3,"lost":2,"tied":1}},"deterministic_floor":{"passed":8,"total":9},"results_file":"..."}
```

Append-only; the HTML report's trend chart reads this file. Rows are only
appended when at least one scenario was graded (no meaningless `0.0`
trend points). Safe to commit or to keep gitignored — your call.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `all 3 run(s) of scenario X errored: no judge model configured` | Pass `--judge-model provider/id`, or set `default_judge_model` in `.steve.eval.jsonc`, or declare `judge_model` in the scenario.toml. |
| Exit code 2 with `warning: no scenarios were graded` | Did you freeze a baseline for this scenario+model? Run `steve eval baseline freeze --scenario X --model Y` first. |
| `user_turns drifted from baseline` (Skipped) | Scenario's `user_turns` changed since the baseline was frozen. Re-freeze: `steve eval baseline freeze --scenario X --model Y`. |
| `scenario.toml not found at ...` (Skipped) | The results file references a scenario that no longer exists under `eval/scenarios/`. Either restore the scenario or regenerate the results file. |
| `regression threshold ... is not a finite number` | Don't pass `NaN`/`Infinity` to `--regression-threshold`. |
| ``baselines dir at <path> exists but is a symlink or non-directory`` | Refuses to write through symlinks (would escape the repo). Remove the symlink or pass `--baselines-dir /real/path` (report only). |
| ``baselines dir from --baselines-dir CLI flag contains `..` components`` | Relative paths with `..` are rejected. Use an absolute path or a non-escaping relative path. |
| Reports look "fine" but obviously regressed runs | Check `--verbose` output for `Skipped` scenarios — if all are skipped, exit will be 2 and the headline is the meaningless `0.0` sentinel. |

## Further reading

- **Design rationale** (the *why*): [`docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md`](./docs/superpowers/specs/2026-05-06-eval-harness-paired-comparison-pivot.md)
- **Maintainer notes** for editing the harness itself: [`src/eval/CLAUDE.md`](./src/eval/CLAUDE.md)
- **Internal validation log** for the modified-scenario subset that
  validates the harness against known failure modes:
  [`eval/VALIDATION.md`](./eval/VALIDATION.md)
- **Canonical schemas in code**:
  - CLI surface: [`src/main.rs`](./src/main.rs)
  - Eval config: [`src/config/eval.rs`](./src/config/eval.rs)
  - Scenario schema: [`src/eval/scenario.rs`](./src/eval/scenario.rs)
  - Baseline + manifest: [`src/eval/baseline.rs`](./src/eval/baseline.rs)
  - Scoring axes + formulas: [`src/eval/score.rs`](./src/eval/score.rs), [`src/eval/report.rs`](./src/eval/report.rs)
  - History row schema: [`src/eval/history.rs`](./src/eval/history.rs)
