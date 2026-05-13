# Eval Module

Scenario manifests live in `eval/scenarios/<name>/scenario.toml` with sibling
fixture files. The walking test
`scenario::tests::all_committed_scenarios_parse_and_validate` runs at `cargo
test` time: parses every `scenario.toml` via `Scenario::from_file`, asserts
each `copy_fixtures` entry exists and is a regular file (via
`symlink_metadata` + `is_file()`, mirroring `ScenarioWorkspace::build`'s
symlink rejection), and pins `_smoke` is in the parsed set.

## Scenario assertion-design pitfalls

- **`RequiresPriorRead(target, ...)` is vacuously satisfied** when `target`
  was never called. Pair with `tool_called(target)` whenever the scenario
  REQUIRES the target tool to fire.
- **`MaxRepeatAttempts` dedups by tool + canonical-args JSON.** Catches
  literal-repeat loops; does NOT catch "agent loops with different commands"
  (the postmortem hypothesis-spinning pattern). Use Judge for count-style
  failure modes. Count-only `MaxToolCalls` primitive tracked: `steve-c0uk`.
- **`tool_not_called(X)` is brittle** — almost any tool has a legitimate
  fallback role. Prefer `tool_called(preferred)` + outcome-pinning via
  `file_contains` on the post-edit file content. Outcome-pinning is robust
  across "agent picked the right tool" AND "agent's preferred-tool call
  failed and it fell back."
- **`is_read_class()` is intentionally narrow** (`Read | Symbols` only) for
  `RequiresPriorRead`. For "did the agent see the content at all?" (where
  `grep` would also count) use `final_message_contains` on an unguessable
  sentinel + Judge instead.
- **Read accepts `path` (string) XOR `paths` (array).** Evaluator's
  `read_path_args` (in `expectations.rs`) handles both forms — Read-specific
  branch. Adding a new multi-path tool requires updating that helper.

## CLI subcommand patterns (`eval/cli.rs`)

- **EPIPE-safe stdout** via `write_stdout_lossy` / `writeln_stdout_lossy`
  / `flush_stdout_lossy`. Plain `print!`/`println!` panic at exit 101
  on broken pipe (`steve eval … | head`), breaking the Pass/Regression/
  NoData exit-code contract.
- **Side effects BEFORE stdout text**: in `report_subcommand`, history
  append and `--html` write run before any stdout write — otherwise
  EPIPE-panic strands user-requested file outputs.
- **`eprintln!`, not `tracing::warn!`, for operator-visible warnings**.
  `main.rs` wires tracing to a file appender (TUI's stdout-ownership
  decision applies binary-wide). Anything an eval CLI operator needs
  to see goes to stderr directly.
- **Exit codes**: `Pass=0`, `Regression=1`, `NoData=2` (no scenarios
  graded), infra error → `Err` → exit 2 via `main`'s outer handler.
  Distinct exit-code-2 conditions share the wire format but MUST stay
  distinct in the type system — reusing `ReportExitCode::NoData` for
  malformed input (NaN threshold, etc.) conflates semantics.

## Comment hygiene

- Inline comments claiming an issue is filed (`tracked separately as a
  follow-up`, etc.) must reference a real `steve-XXXX` ID inline. Vague
  claims rot into false tracking; ID references are checkable. Same rule
  applies to commit-message bodies.
- Same rule applies to **review-event attributions** — "the bug Copilot
  caught", "round-3 fixed", "previously", commit-hash citations. State
  the durable invariant ("collapsing X into Y would make Y beat Z"),
  not the catalyst. Review-event references rot into meaningless
  pointers once threads archive or commits squash.
- For `FileType` in panic messages, use `describe_file_type` helper (in the
  scenario.rs test module) — `Debug` impl prints raw `st_mode` bits which
  no human reads at panic time.
