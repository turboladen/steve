//! LLM-as-judge assertion runner.
//!
//! Phase 3's rule-based evaluator handles structural facts (tool sequence,
//! file diffs); behavioral checks like "did the assistant give up" or
//! "did it fabricate tool output" don't reduce cleanly to substring
//! matches, so the schema reserves [`Expectation::Judge`] for an LLM-graded
//! verdict. This module performs that grading.
//!
//! Architecture: a small [`JudgeBackend`] trait is the test seam — the
//! production [`RegistryBackend`] talks to a provider via
//! [`crate::provider::ProviderRegistry`], while the unit tests in this file
//! use a `MockBackend` returning canned `(text, usage)` pairs or transport
//! errors. All response handling is in the pure [`build_judge_outcome`]
//! function so failure-mode tests don't need any provider plumbing at all.
//!
//! Two non-obvious design choices worth highlighting:
//!
//! - [`validate_judge_config`] runs at the CLI seam **before** the agent
//!   run starts, mirroring the API-key check in `Runner::build`. A
//!   misconfigured judge fails fast instead of burning the scenario's
//!   token budget only to surface the problem in the final JSON.
//!
//! - The user prompt uses **head + tail windowing** for both tool calls
//!   and assistant messages. A "first N" cap would lose the resolution
//!   turns — the very content most judge rubrics grade on. Splitting the
//!   budget shows the opening AND the resolution with a sentinel marking
//!   the elided middle (see `MAX_*_HEAD` / `MAX_*_TAIL` constants).

use std::borrow::Cow;

use anyhow::{Context, Result};
use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, CreateChatCompletionRequest,
};
use async_trait::async_trait;
use serde::Deserialize;

use crate::{
    eval::{
        capture::{CapturedRun, RecordedToolCall},
        expectations::{EvalReport, JudgeRecord, Outcome},
        scenario::{Expectation, Scenario},
    },
    event::StreamUsage,
    provider::ProviderRegistry,
    truncate_chars,
};

/// One judge invocation's result. `JudgeVerdict` (rather than `Outcome`)
/// makes the "judges always commit" rule a type-system property: a judge
/// cannot produce a `Skipped` value at all. `apply_judges` lifts the
/// verdict back into `Outcome` at the result-recording site.
#[derive(Debug)]
pub struct JudgeOutcome {
    pub verdict: JudgeVerdict,
    pub record: JudgeRecord,
}

/// What a judge can decide. Restricted to two variants so a future
/// rewrite of the judging code can't accidentally produce `Skipped` and
/// silently turn a behavioral check into a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeVerdict {
    Passed,
    Failed { reason: String },
}

impl JudgeVerdict {
    /// Lift the judge-only verdict into the broader `Outcome` shape used
    /// by `EvalReport.results`. Always lossless — every `JudgeVerdict` is
    /// representable in `Outcome`.
    pub fn into_outcome(self) -> Outcome {
        match self {
            JudgeVerdict::Passed => Outcome::Passed,
            JudgeVerdict::Failed { reason } => Outcome::Failed { reason },
        }
    }
}

/// System prompt sent to every judge call. Holds the output contract the
/// JSON parser depends on; rewording it without updating the parser will
/// silently break verdicts.
const SYSTEM_PROMPT: &str = "\
You are an evaluator for an AI coding agent's transcript. You will be given:
1. PASS_WHEN: a criterion that must be true for the scenario to pass.
2. FAIL_WHEN: a disqualifying condition that must NOT be true.
3. The captured run: assistant messages and tool calls in chronological order.

Return ONLY a JSON object with no markdown fences, no preamble, no trailing text:
{\"passed\": <bool>, \"reason\": \"<one or two sentences citing specific evidence>\"}

passed=true requires PASS_WHEN to hold AND FAIL_WHEN to NOT hold. Cite the
evidence (turn number, tool name, message excerpt) that drove your decision.";

const MAX_ASSISTANT_MSG_CHARS: usize = 4096;
/// Head + tail window sizes for assistant-message truncation. A head-only
/// "first N" cap loses the *resolution* turns — judge prompts in this
/// codebase typically ask "did the agent eventually surrender / get back
/// on track", so the final turns matter more than the opening. Splitting
/// the budget shows both the start and the resolution with a sentinel
/// marking the elided middle.
const MAX_ASSISTANT_MSGS_HEAD: usize = 10;
const MAX_ASSISTANT_MSGS_TAIL: usize = 15;
const MAX_TOOL_ARGS_CHARS: usize = 1024;
const MAX_TOOL_OUTPUT_CHARS: usize = 2048;
const MAX_RAW_RESPONSE_IN_REASON: usize = 500;
/// Cap on the number of validate_judge_config failures shown verbatim
/// before a "... (N more truncated)" sentinel kicks in. Prevents a
/// scenario with dozens of misconfigured judges from producing a
/// terminal-flooding error.
const MAX_VALIDATION_ERRORS_SHOWN: usize = 10;

/// Schema of the judge model's expected JSON output.
#[derive(Debug, Deserialize)]
struct JudgeResponse {
    passed: bool,
    reason: String,
}

// ──────────────────────────────────────────────────────────────────────
// Backend trait — the test seam.
// ──────────────────────────────────────────────────────────────────────

/// Boundary between Phase 4's orchestration and the actual chat provider
/// call. Production wires [`RegistryBackend`]; tests substitute their own
/// implementation returning canned `(String, Option<StreamUsage>)` pairs
/// or transport errors.
#[async_trait]
pub(crate) trait JudgeBackend: Send + Sync {
    async fn complete(
        &self,
        model_ref: &str,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<(String, Option<StreamUsage>)>;
}

/// Production backend: resolves the model through the registry, builds an
/// async-openai non-streaming chat request, and returns the assistant's
/// content + usage. `pub(crate)` because it's only constructed inside the
/// module (via `Judge::from_registry`); external callers go through `Judge`.
pub(crate) struct RegistryBackend<'a> {
    registry: &'a ProviderRegistry,
}

impl<'a> RegistryBackend<'a> {
    pub(crate) fn new(registry: &'a ProviderRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl JudgeBackend for RegistryBackend<'_> {
    async fn complete(
        &self,
        model_ref: &str,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<(String, Option<StreamUsage>)> {
        let resolved = self
            .registry
            .resolve_model(model_ref)
            .with_context(|| format!("judge model not resolvable: {model_ref:?}"))?;
        let client = self
            .registry
            .client(&resolved.provider_id)
            .with_context(|| {
                format!(
                    "provider {:?} not configured for judge",
                    resolved.provider_id
                )
            })?;

        let request = CreateChatCompletionRequest {
            model: resolved.api_model_id().to_string(),
            messages: vec![
                ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(
                        system_prompt.to_string(),
                    ),
                    name: None,
                }),
                ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                    content: ChatCompletionRequestUserMessageContent::Text(user_prompt.to_string()),
                    name: None,
                }),
            ],
            temperature: Some(0.0),
            ..Default::default()
        };

        let response = client
            .inner()
            .chat()
            .create(request)
            .await
            .context("judge chat completion request failed")?;

        let text = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        let usage = response.usage.map(|u| StreamUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        Ok((text, usage))
    }
}

// ──────────────────────────────────────────────────────────────────────
// Judge orchestrator.
// ──────────────────────────────────────────────────────────────────────

/// Drives one Judge expectation through model resolution, prompt building,
/// the backend call, and outcome processing. Holds an owned `cli_model`
/// (rather than a borrow) because the value is set once at startup from
/// the CLI flag and outlives every other reference; using `Option<String>`
/// removes a lifetime parameter that would otherwise force the backend
/// borrow and the CLI string borrow to share a scope unnecessarily.
pub struct Judge<'a> {
    backend: Box<dyn JudgeBackend + 'a>,
    cli_model: Option<String>,
}

impl<'a> Judge<'a> {
    /// Production constructor: wraps a registry into a [`RegistryBackend`].
    pub fn from_registry(registry: &'a ProviderRegistry, cli_model: Option<&str>) -> Self {
        Self {
            backend: Box::new(RegistryBackend::new(registry)),
            cli_model: cli_model.map(str::to_owned),
        }
    }

    /// Test/internal constructor: accept any backend implementation.
    pub(crate) fn with_backend(
        backend: Box<dyn JudgeBackend + 'a>,
        cli_model: Option<&str>,
    ) -> Self {
        Self {
            backend,
            cli_model: cli_model.map(str::to_owned),
        }
    }

    /// Evaluate one Judge expectation. Takes the destructured judge fields
    /// directly (rather than a generic `&Expectation`) so the type system
    /// rules out non-Judge inputs at compile time — there's no defensive
    /// runtime branch to test or maintain.
    pub async fn evaluate(
        &self,
        scenario_judge_model: Option<&str>,
        pass_when: &str,
        fail_when: &str,
        expectation_judge_model: Option<&str>,
        captured: &CapturedRun,
    ) -> JudgeOutcome {
        let user_prompt = build_user_prompt(pass_when, fail_when, captured);

        let model = match resolve_judge_model(
            self.cli_model.as_deref(),
            expectation_judge_model,
            scenario_judge_model,
        ) {
            Some(m) => m.to_string(),
            None => {
                return JudgeOutcome {
                    verdict: JudgeVerdict::Failed {
                        reason: "no judge model configured: pass --judge-model on the CLI, \
                                 or set judge_model on the scenario or per-expectation"
                            .into(),
                    },
                    record: JudgeRecord {
                        model: None,
                        system_prompt: SYSTEM_PROMPT.into(),
                        user_prompt,
                        raw_response: String::new(),
                        usage: None,
                    },
                };
            }
        };

        let response = self
            .backend
            .complete(&model, SYSTEM_PROMPT, &user_prompt)
            .await;

        build_judge_outcome(&model, SYSTEM_PROMPT, &user_prompt, response)
    }
}

/// Up-front validation that every Judge expectation in `scenario` will
/// be able to resolve a real judge model — checked **before** the agent
/// run starts, so a misconfigured judge fails fast instead of burning
/// the scenario's token budget only to surface the problem in the
/// final JSON.
///
/// Mirrors `Runner::build`'s API-key validation: a Judge expectation
/// with no resolvable model is a config error of the same kind (an
/// invariant we can check from configuration alone, no I/O needed).
///
/// Two failure modes:
/// 1. No model set anywhere (CLI flag + per-expectation + scenario all `None`).
/// 2. A model is set but doesn't resolve through `judge_registry` — typo,
///    missing provider entry in the user's config, etc.
///
/// Returns `Ok(())` when the scenario has no Judge expectations at all.
pub fn validate_judge_config(
    scenario: &Scenario,
    judge_registry: &ProviderRegistry,
    cli_model: Option<&str>,
) -> Result<()> {
    // Collect every Judge expectation's failure rather than bailing on
    // the first — a scenario with two typo'd judge_models should report
    // both at once, so the user only re-runs validation once instead of
    // hitting the same fail-fast wall twice.
    let mut failures: Vec<String> = Vec::new();
    for (idx, exp) in scenario.expectations.iter().enumerate() {
        let Expectation::Judge { judge_model, .. } = exp else {
            continue;
        };
        let effective = resolve_judge_model(
            cli_model,
            judge_model.as_deref(),
            scenario.judge_model.as_deref(),
        );
        let Some(model) = effective else {
            failures.push(format!(
                "expectation #{} (kind = \"judge\") has no model configured. \
                 Pass --judge-model on the CLI, or set judge_model on the scenario \
                 or per-expectation.",
                idx + 1
            ));
            continue;
        };
        if let Err(err) = judge_registry.resolve_model(model) {
            failures.push(format!(
                "expectation #{} (kind = \"judge\") requires judge model {model:?}, \
                 which is not resolvable from the configured providers: {err:#}",
                idx + 1
            ));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    let total = failures.len();
    let shown = total.min(MAX_VALIDATION_ERRORS_SHOWN);
    let mut listing: Vec<String> = failures.into_iter().take(shown).collect();
    if total > shown {
        listing.push(format!(
            "... ({} more issue{} truncated)",
            total - shown,
            if total - shown == 1 { "" } else { "s" }
        ));
    }
    let header_suffix = if total > shown {
        format!(", showing first {shown}")
    } else {
        String::new()
    };
    Err(anyhow::anyhow!(
        "judge config invalid ({} issue{}{}):\n  - {}",
        total,
        if total == 1 { "" } else { "s" },
        header_suffix,
        listing.join("\n  - ")
    ))
}

/// Walk `report.results`, replacing every `Expectation::Judge` entry's
/// outcome (currently `Skipped` from the Phase 3 evaluator) with the
/// judge's verdict and populating `judge: Some(record)`.
pub async fn apply_judges(
    report: &mut EvalReport,
    scenario: &Scenario,
    captured: &CapturedRun,
    judge: &Judge<'_>,
) {
    for result in &mut report.results {
        // Exhaustive match (rather than a `let Expectation::Judge {...} =
        // ... else { continue }`) so adding a new judging-class variant in
        // the future is a compile error here, not a silent skip — Phase 3
        // produced `Outcome::Skipped` for unhandled variants, which the
        // report treats as neutral, so a missed match would silently turn
        // a behavioral check into a no-op.
        let (pass_when, fail_when, judge_model) = match &result.expectation {
            Expectation::Judge {
                pass_when,
                fail_when,
                judge_model,
            } => (pass_when, fail_when, judge_model),
            Expectation::ToolCalled { .. }
            | Expectation::ToolNotCalled { .. }
            | Expectation::RequiresPriorRead { .. }
            | Expectation::FileUnchanged { .. }
            | Expectation::FileContains { .. }
            | Expectation::FinalMessageContains { .. }
            | Expectation::FinalMessageNotContains { .. }
            | Expectation::MaxRepeatAttempts { .. } => continue,
        };
        let JudgeOutcome { verdict, record } = judge
            .evaluate(
                scenario.judge_model.as_deref(),
                pass_when,
                fail_when,
                judge_model.as_deref(),
                captured,
            )
            .await;
        result.outcome = verdict.into_outcome();
        result.judge = Some(record);
    }
}

// ──────────────────────────────────────────────────────────────────────
// Pure helpers — fully unit-testable.
// ──────────────────────────────────────────────────────────────────────

/// CLI > per-expectation > scenario default. Returns `None` if no source
/// is set, signaling the caller to emit the "no judge model configured"
/// failure.
pub(crate) fn resolve_judge_model<'a>(
    cli_override: Option<&'a str>,
    per_expectation: Option<&'a str>,
    scenario_default: Option<&'a str>,
) -> Option<&'a str> {
    cli_override.or(per_expectation).or(scenario_default)
}

/// Truncate `s` to at most `max` characters with a bounded walk —
/// `truncate_chars` does an O(n) `chars().count()` up front, so feeding
/// it a multi-MB string costs O(MB) just to determine the length even
/// though we only need the first `max` chars. This helper reads at most
/// `max + 1` chars before deciding, returning a `Cow::Borrowed` when no
/// truncation is needed (no allocation in the common case).
///
/// Used by the JSON-args formatter to avoid serializing the full body
/// of `edit`/`write` tool arguments whose `content` / `new_string`
/// fields can be very large.
fn truncate_str_bounded(s: &str, max: usize) -> Cow<'_, str> {
    // Byte length is an upper bound on char count (each char ≥ 1 byte),
    // so a short ASCII string skips all walking.
    if s.len() <= max {
        return Cow::Borrowed(s);
    }
    let cut_n = if max >= 4 { max - 3 } else { max };
    let mut iter = s.char_indices();
    let cut_byte = match iter.by_ref().nth(cut_n) {
        Some((pos, _)) => pos,
        None => return Cow::Borrowed(s), // fewer than cut_n+1 chars ≤ max
    };
    // Need to see (max - cut_n) more chars to confirm we exceed max total.
    let extra_needed = max - cut_n;
    let extra_found = iter.take(extra_needed).count();
    if extra_found < extra_needed {
        Cow::Borrowed(s)
    } else if max >= 4 {
        Cow::Owned(format!("{}...", &s[..cut_byte]))
    } else {
        Cow::Owned(s[..cut_byte].to_string())
    }
}

/// Format a tool-call's `arguments` JSON value into a compact string,
/// truncating each leaf string to `max_str_chars` before serialization.
///
/// This avoids `value.to_string()`'s unbounded allocation: an `edit` or
/// `write` tool with a multi-MB `content`/`new_string` field would
/// otherwise allocate the full payload as JSON before truncation
/// discarded all but ~1KB of it. Here, each large string is replaced
/// with its truncated form before any JSON output is built, so total
/// allocation is bounded by `num_string_fields × max_str_chars` rather
/// than the original payload size.
fn format_args_compact(v: &serde_json::Value, max_str_chars: usize, out: &mut String) {
    match v {
        serde_json::Value::String(s) => {
            let trunc = truncate_str_bounded(s, max_str_chars);
            // serde_json handles JSON escape correctly. Allocation is
            // bounded by the truncated string's length.
            if let Ok(escaped) = serde_json::to_string(trunc.as_ref()) {
                out.push_str(&escaped);
            } else {
                out.push_str("\"\"");
            }
        }
        serde_json::Value::Array(arr) => {
            out.push('[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                format_args_compact(item, max_str_chars, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(obj) => {
            out.push('{');
            for (i, (k, val)) in obj.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if let Ok(escaped_key) = serde_json::to_string(k) {
                    out.push_str(&escaped_key);
                }
                out.push(':');
                format_args_compact(val, max_str_chars, out);
            }
            out.push('}');
        }
        // Number, Bool, Null are bounded — serialize directly.
        _ => {
            if let Ok(s) = serde_json::to_string(v) {
                out.push_str(&s);
            }
        }
    }
}

/// Strip ` ```json ... ``` ` (or plain ` ``` ... ``` `) fences if the
/// model wrapped its JSON despite instructions. Returns the original
/// string when no balanced fence is present.
pub(crate) fn strip_markdown_fences(s: &str) -> &str {
    let trimmed = s.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    body.trim_start_matches('\n')
        .strip_suffix("```")
        .map(|b| b.trim_end_matches('\n'))
        .unwrap_or(body)
        .trim()
}

/// Build the user prompt for the judge. Truncates per-tool-call args/output
/// and per-message body to keep the prompt bounded.
///
/// Layout matches real execution order **per turn**: each turn shows its
/// tool calls (in emit order) followed by the assistant's final message.
/// Turns appear in chronological order. Round-3 fixed the within-turn
/// layout (tool calls before message); this rendering also fixes
/// across-turn layout (turn N's calls before turn N+1's calls), which a
/// flat "all tool calls then all messages" presentation broke for
/// multi-turn scenarios.
///
/// Truncation happens at the TURN level — when a run has more turns than
/// fit, the head and tail turns are shown with a sentinel naming the
/// elided range. Within a kept turn, every tool call is rendered (the
/// per-tool-call args/output truncation still bounds individual entries).
/// This is simpler than two-tier truncation and handles the common case
/// (1-10 turns, 1-20 calls each) well.
pub(crate) fn build_user_prompt(
    pass_when: &str,
    fail_when: &str,
    captured: &CapturedRun,
) -> String {
    let mut out = String::new();
    out.push_str("PASS_WHEN: ");
    out.push_str(pass_when);
    out.push_str("\nFAIL_WHEN: ");
    out.push_str(fail_when);
    out.push_str("\n\nCAPTURED RUN — turns in chronological order. ");
    out.push_str(
        "Each turn shows its tool calls (in emit order) FIRST, then the assistant's \
         final message for that turn.\n",
    );

    // Group tool calls by their turn_index. We may also see "orphan"
    // calls (turn_index >= assistant_messages.len()) when the final turn
    // never finished — their bucket has no message but is still rendered
    // so the trace stays complete.
    let max_finished_turn = captured.assistant_messages.len();
    let max_observed_turn = captured
        .tool_calls
        .iter()
        .map(|c| c.turn_index + 1)
        .max()
        .unwrap_or(0)
        .max(max_finished_turn);

    if max_observed_turn == 0 {
        out.push_str("\n(no turns recorded)\n\n");
    } else {
        let mut by_turn: Vec<Vec<&RecordedToolCall>> =
            (0..max_observed_turn).map(|_| Vec::new()).collect();
        for call in &captured.tool_calls {
            // Defensive: a malformed capture with turn_index past the
            // end shouldn't panic — drop it into the last bucket so the
            // call is still visible.
            let bucket = call.turn_index.min(max_observed_turn - 1);
            by_turn[bucket].push(call);
        }

        let render_turn = |turn_idx: usize,
                           calls: &[&RecordedToolCall],
                           message: Option<&String>,
                           out: &mut String| {
            out.push_str(&format!("\nTurn {}:\n", turn_idx + 1));
            if calls.is_empty() {
                out.push_str("  Tool calls: (none)\n");
            } else {
                out.push_str("  Tool calls (in emit order):\n");
                for (i, call) in calls.iter().enumerate() {
                    // format_args_compact bounds per-string allocation; the
                    // outer truncate_chars adds the "..." sentinel and caps
                    // the total size when many fields combine.
                    let mut compact = String::new();
                    format_args_compact(&call.arguments, MAX_TOOL_ARGS_CHARS, &mut compact);
                    let args = truncate_chars(&compact, MAX_TOOL_ARGS_CHARS);
                    let output = match &call.output {
                        Some(o) if call.is_error => {
                            format!("(error) {}", truncate_chars(o, MAX_TOOL_OUTPUT_CHARS))
                        }
                        Some(o) => truncate_chars(o, MAX_TOOL_OUTPUT_CHARS),
                        None => "(no output recorded)".to_string(),
                    };
                    out.push_str(&format!(
                        "    {}. {}({}) -> {}\n",
                        i + 1,
                        call.tool_name.as_str(),
                        args,
                        output
                    ));
                }
            }
            match message {
                Some(msg) => {
                    out.push_str("  Final assistant message:\n    ");
                    let truncated = truncate_chars(msg, MAX_ASSISTANT_MSG_CHARS);
                    // Indent continuation lines under "    " for readability.
                    out.push_str(&truncated.replace('\n', "\n    "));
                    out.push('\n');
                }
                None => {
                    out.push_str("  (turn did not finish; no final assistant message recorded)\n");
                }
            }
        };

        let head = MAX_ASSISTANT_MSGS_HEAD;
        let tail = MAX_ASSISTANT_MSGS_TAIL;
        let render = |i: usize, out: &mut String| {
            render_turn(i, &by_turn[i], captured.assistant_messages.get(i), out);
        };
        if max_observed_turn <= head + tail {
            for i in 0..max_observed_turn {
                render(i, &mut out);
            }
        } else {
            for i in 0..head {
                render(i, &mut out);
            }
            let dropped = max_observed_turn - head - tail;
            out.push_str(&format!(
                "\n... ({} more turn{} truncated between Turn {} and Turn {}) ...\n",
                dropped,
                if dropped == 1 { "" } else { "s" },
                head,
                max_observed_turn - tail + 1
            ));
            for i in (max_observed_turn - tail)..max_observed_turn {
                render(i, &mut out);
            }
        }
        out.push('\n');
    }

    out.push_str(&format!("Errors: {:?}\n", captured.errors));
    out.push_str(&format!("Timed out: {}\n", captured.timed_out));
    out
}

/// Pure response processor: every failure path lands here so unit tests
/// can cover all branches without standing up a provider.
pub(crate) fn build_judge_outcome(
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    response: Result<(String, Option<StreamUsage>)>,
) -> JudgeOutcome {
    let make_record = |raw_response: String, usage: Option<StreamUsage>| JudgeRecord {
        model: Some(model.to_string()),
        system_prompt: system_prompt.to_string(),
        user_prompt: user_prompt.to_string(),
        raw_response,
        usage,
    };

    let (raw, usage) = match response {
        Ok(pair) => pair,
        Err(e) => {
            return JudgeOutcome {
                verdict: JudgeVerdict::Failed {
                    reason: format!("judge call failed: {e:#}"),
                },
                record: make_record(String::new(), None),
            };
        }
    };

    if raw.trim().is_empty() {
        return JudgeOutcome {
            verdict: JudgeVerdict::Failed {
                reason: "judge returned empty response".into(),
            },
            record: make_record(raw, usage),
        };
    }

    let stripped = strip_markdown_fences(&raw);
    match serde_json::from_str::<JudgeResponse>(stripped) {
        Ok(parsed) => {
            let verdict = if parsed.passed {
                JudgeVerdict::Passed
            } else if parsed.reason.trim().is_empty() {
                // A failure with no diagnostic is itself a silent failure
                // — the user sees `"status": "failed"` with an empty
                // reason and learns nothing. Surface the raw response so
                // they can see what the judge actually emitted.
                let snippet = truncate_chars(&raw, MAX_RAW_RESPONSE_IN_REASON);
                JudgeVerdict::Failed {
                    reason: format!(
                        "judge returned passed=false with empty reason; raw response: {snippet}"
                    ),
                }
            } else {
                JudgeVerdict::Failed {
                    reason: parsed.reason,
                }
            };
            JudgeOutcome {
                verdict,
                record: make_record(raw, usage),
            }
        }
        Err(e) => {
            let snippet = truncate_chars(&raw, MAX_RAW_RESPONSE_IN_REASON);
            JudgeOutcome {
                verdict: JudgeVerdict::Failed {
                    reason: format!("judge returned invalid JSON: {e:#}; raw response: {snippet}"),
                },
                record: make_record(raw, usage),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, path::PathBuf, sync::Mutex};

    use serde_json::json;

    use super::*;
    use crate::{
        eval::{
            capture::{CapturedRun, RecordedToolCall},
            expectations::{EvalReport, ExpectationResult},
            scenario::{Expectation, Scenario, Setup},
            workspace::WorkspaceSnapshot,
        },
        tool::ToolName,
    };

    /// One queued canned response for the mock backend: either a
    /// `(text, usage)` pair or a transport-style error.
    type CannedResponse = Result<(String, Option<StreamUsage>)>;

    /// Test backend: returns canned responses (success or transport error)
    /// in the order they were queued. Uses a `VecDeque` + `pop_front` so
    /// multi-response tests get FIFO order — a `Vec` + `pop` would silently
    /// reverse the queue.
    struct MockBackend {
        responses: Mutex<VecDeque<CannedResponse>>,
    }

    impl MockBackend {
        fn new(responses: Vec<CannedResponse>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
            }
        }
    }

    #[async_trait]
    impl JudgeBackend for MockBackend {
        async fn complete(
            &self,
            _model_ref: &str,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> CannedResponse {
            self.responses
                .lock()
                .expect("mock lock poisoned")
                .pop_front()
                .expect("MockBackend out of canned responses")
        }
    }

    fn empty_capture() -> CapturedRun {
        CapturedRun::new(
            PathBuf::from("/tmp/eval-test"),
            WorkspaceSnapshot {
                files: Default::default(),
            },
        )
    }

    fn judge_expectation(model: Option<&str>) -> Expectation {
        Expectation::Judge {
            pass_when: "the assistant did the right thing".into(),
            fail_when: "the assistant gave up".into(),
            judge_model: model.map(|s| s.to_string()),
        }
    }

    fn scenario_with(scenario_judge_model: Option<&str>, expectation: Expectation) -> Scenario {
        Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: vec![expectation],
            judge_model: scenario_judge_model.map(|s| s.to_string()),
        }
    }

    fn ok_response(raw: &str) -> CannedResponse {
        Ok((
            raw.to_string(),
            Some(StreamUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
            }),
        ))
    }

    /// Variant of `ok_response` for OpenAI-compatible providers that
    /// return success but do not report token usage. Steve's
    /// `stream_options.include_usage` request flag is required for
    /// Anthropic-via-OpenAI-compat to report usage; some other
    /// compat-layer providers honor it inconsistently.
    fn ok_response_no_usage(raw: &str) -> CannedResponse {
        Ok((raw.to_string(), None))
    }

    // ── Pure: resolve_judge_model ──

    #[test]
    fn cli_model_takes_precedence_over_per_expectation() {
        let m = resolve_judge_model(Some("cli/x"), Some("per/y"), Some("scn/z"));
        assert_eq!(m, Some("cli/x"));
    }

    #[test]
    fn per_expectation_takes_precedence_over_scenario() {
        let m = resolve_judge_model(None, Some("per/y"), Some("scn/z"));
        assert_eq!(m, Some("per/y"));
    }

    #[test]
    fn scenario_default_used_when_no_other_override() {
        let m = resolve_judge_model(None, None, Some("scn/z"));
        assert_eq!(m, Some("scn/z"));
    }

    #[test]
    fn no_model_anywhere_returns_none() {
        let m = resolve_judge_model(None, None, None);
        assert!(m.is_none());
    }

    // ── Pure: strip_markdown_fences ──

    #[test]
    fn strip_fences_passthrough_when_no_fences() {
        let s = r#"{"passed": true, "reason": "ok"}"#;
        assert_eq!(strip_markdown_fences(s), s);
    }

    #[test]
    fn strip_fences_removes_json_fence() {
        let s = "```json\n{\"passed\": true, \"reason\": \"ok\"}\n```";
        assert_eq!(
            strip_markdown_fences(s),
            r#"{"passed": true, "reason": "ok"}"#
        );
    }

    #[test]
    fn strip_fences_removes_plain_fence() {
        let s = "```\n{\"passed\": false}\n```";
        assert_eq!(strip_markdown_fences(s), r#"{"passed": false}"#);
    }

    #[test]
    fn strip_fences_handles_leading_trailing_whitespace() {
        let s = "  \n```json\n{\"a\":1}\n```\n  ";
        assert_eq!(strip_markdown_fences(s), r#"{"a":1}"#);
    }

    // ── Pure: build_judge_outcome ──

    #[test]
    fn judge_outcome_passed_on_passed_true() {
        let raw = r#"{"passed": true, "reason": "looks good"}"#;
        let r = ok_response(raw);
        let out = build_judge_outcome("m", "sys", "user", r);
        assert!(matches!(out.verdict, JudgeVerdict::Passed));
        assert_eq!(out.record.model.as_deref(), Some("m"));
        assert!(out.record.usage.is_some());
        // Pin raw_response on the success path too — Phase 6's compare
        // differ relies on it for reproducibility, and a regression that
        // populated raw_response only on the Failed branch would slip
        // through tests that only check `usage.is_some()`.
        assert_eq!(out.record.raw_response, raw);
    }

    #[test]
    fn judge_outcome_passed_when_provider_omits_usage() {
        // Some OpenAI-compatible providers don't honor
        // `stream_options.include_usage` and return success with no usage
        // numbers. The judge must still produce a clean verdict, and the
        // record must carry `usage: None` rather than panicking on a
        // missing-usage assumption (no `.unwrap()` on usage anywhere).
        let raw = r#"{"passed": true, "reason": "ok"}"#;
        let r = ok_response_no_usage(raw);
        let out = build_judge_outcome("m", "sys", "user", r);
        assert!(matches!(out.verdict, JudgeVerdict::Passed));
        assert_eq!(out.record.model.as_deref(), Some("m"));
        assert!(
            out.record.usage.is_none(),
            "usage must be None when provider doesn't report it"
        );
        assert_eq!(out.record.raw_response, raw);
    }

    #[test]
    fn judge_outcome_failed_on_passed_false() {
        let r = ok_response(r#"{"passed": false, "reason": "agent surrendered"}"#);
        let out = build_judge_outcome("m", "sys", "user", r);
        match out.verdict {
            JudgeVerdict::Failed { reason } => assert_eq!(reason, "agent surrendered"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn judge_outcome_failed_on_invalid_json() {
        let r = ok_response("definitely not json at all");
        let out = build_judge_outcome("m", "sys", "user", r);
        match out.verdict {
            JudgeVerdict::Failed { reason } => {
                assert!(reason.contains("invalid JSON"), "reason: {reason}");
                assert!(
                    reason.contains("definitely not json"),
                    "reason should embed raw response: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(out.record.raw_response, "definitely not json at all");
    }

    #[test]
    fn judge_outcome_strips_markdown_fence_then_parses() {
        let r = ok_response("```json\n{\"passed\": true, \"reason\": \"good\"}\n```");
        let out = build_judge_outcome("m", "sys", "user", r);
        assert!(
            matches!(out.verdict, JudgeVerdict::Passed),
            "fence should strip and JSON should parse"
        );
    }

    #[test]
    fn judge_outcome_failed_on_transport_error() {
        let r: Result<(String, Option<StreamUsage>)> =
            Err(anyhow::anyhow!("connection refused")).context("contacting judge endpoint");
        let out = build_judge_outcome("m", "sys", "user", r);
        match out.verdict {
            JudgeVerdict::Failed { reason } => {
                assert!(reason.contains("judge call failed"), "reason: {reason}");
                // Anyhow chain must surface the inner cause via {err:#}, NOT
                // just the outer context.
                assert!(
                    reason.contains("connection refused"),
                    "alternate-format anyhow should surface inner cause: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(out.record.raw_response.is_empty());
        assert!(out.record.usage.is_none());
    }

    #[test]
    fn judge_outcome_failed_on_empty_response() {
        let r = ok_response("   \n  ");
        let out = build_judge_outcome("m", "sys", "user", r);
        match out.verdict {
            JudgeVerdict::Failed { reason } => {
                assert!(reason.contains("empty response"), "reason: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ── Pure: build_user_prompt ──

    #[test]
    fn prompt_includes_pass_fail_criteria() {
        let cap = empty_capture();
        let out = build_user_prompt("PASSCRIT", "FAILCRIT", &cap);
        assert!(out.contains("PASS_WHEN: PASSCRIT"), "got: {out}");
        assert!(out.contains("FAIL_WHEN: FAILCRIT"), "got: {out}");
    }

    #[test]
    fn prompt_truncates_long_tool_output() {
        let mut cap = empty_capture();
        cap.tool_calls.push(RecordedToolCall {
            call_id: "1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "foo"}),
            output: Some("X".repeat(100_000)),
            is_error: false,
            turn_index: 0,
        });
        let out = build_user_prompt("p", "f", &cap);
        // The full 100k bytes of output must NOT be in the prompt — the
        // per-call cap is well under that.
        assert!(
            out.len() < 20_000,
            "prompt should stay bounded; got {} bytes",
            out.len()
        );
        assert!(out.contains("read("), "tool call header should appear");
    }

    #[test]
    fn prompt_truncates_huge_string_in_tool_args_without_unbounded_alloc() {
        // The original `call.arguments.to_string()` would have allocated
        // a multi-MB JSON string here before truncating. The compact
        // formatter must bound per-string before serialization, so a
        // 5MB content payload yields a prompt under the per-call cap.
        // (This test guards against the regression Copilot caught on
        // PR #53 where `edit`/`write` tools could OOM the prompt.)
        let huge = "Y".repeat(5_000_000);
        let mut cap = empty_capture();
        cap.tool_calls.push(RecordedToolCall {
            call_id: "1".into(),
            tool_name: ToolName::Edit,
            arguments: json!({"path": "src/foo.rs", "new_string": huge}),
            output: Some("ok".into()),
            is_error: false,
            turn_index: 0,
        });
        let out = build_user_prompt("p", "f", &cap);
        // Total prompt must stay small — well under the original 5MB
        // payload. With MAX_TOOL_ARGS_CHARS = 1024 and one truncated
        // string, the args section is ≤ ~1.1KB; the whole prompt
        // (criteria + turn header + args) lands under a few KB.
        assert!(
            out.len() < 5_000,
            "prompt must stay bounded under multi-MB tool args; got {} bytes",
            out.len()
        );
        // The huge string must show signs of truncation, not 5MB of Y's.
        let yyy_count = out.matches('Y').count();
        assert!(
            yyy_count < 1500,
            "huge string must be truncated; saw {yyy_count} Y characters in prompt"
        );
    }

    #[test]
    fn prompt_preserves_small_fields_around_huge_one() {
        // Same scenario as above but with the small field guaranteed to
        // sort BEFORE the huge field alphabetically (serde_json's Map
        // serializes BTreeMap-ordered without the `preserve_order`
        // feature). Pin that small fields render verbatim alongside a
        // truncated large field as long as they fit in the outer cap.
        let huge = "Y".repeat(5_000_000);
        let mut cap = empty_capture();
        cap.tool_calls.push(RecordedToolCall {
            call_id: "1".into(),
            tool_name: ToolName::Edit,
            // "aa_path" sorts before "z_huge", so it renders first.
            arguments: json!({"aa_path": "src/foo.rs", "z_huge": huge}),
            output: Some("ok".into()),
            is_error: false,
            turn_index: 0,
        });
        let out = build_user_prompt("p", "f", &cap);
        assert!(
            out.contains(r#""aa_path":"src/foo.rs""#),
            "small leading field must render verbatim, got prompt slice:\n{}",
            &out[..out.len().min(2000)]
        );
    }

    #[test]
    fn truncate_str_bounded_short_string_borrows() {
        let s = "hello";
        match truncate_str_bounded(s, 100) {
            Cow::Borrowed(b) => assert_eq!(b, "hello"),
            Cow::Owned(_) => panic!("short string should not allocate"),
        }
    }

    #[test]
    fn truncate_str_bounded_long_string_truncates_with_ellipsis() {
        let s = "abcdefghijklmnop"; // 16 chars
        let out = truncate_str_bounded(s, 10);
        // max-3 = 7 chars + "..."
        assert_eq!(out.as_ref(), "abcdefg...");
    }

    #[test]
    fn truncate_str_bounded_exact_cap_borrows() {
        let s = "abcdefghij"; // exactly 10 chars
        match truncate_str_bounded(s, 10) {
            Cow::Borrowed(b) => assert_eq!(b, "abcdefghij"),
            Cow::Owned(_) => panic!("exact-length string should not allocate"),
        }
    }

    #[test]
    fn truncate_str_bounded_unicode_safe() {
        // 6 chars of which several are multi-byte. Bounded at 4 chars +
        // "..." form means we cut at the (max-3)=1st char.
        let s = "αβγδεζ"; // 6 chars, 12 bytes
        let out = truncate_str_bounded(s, 4);
        assert_eq!(out.as_ref(), "α..."); // 1 char + "..."
    }

    #[test]
    fn truncate_str_bounded_does_not_walk_full_string() {
        // Conceptual test: this would OOM or take seconds if the
        // function walked the full string. Allocating a huge string
        // and feeding it should return quickly.
        let huge = "Z".repeat(1_000_000);
        let out = truncate_str_bounded(&huge, 50);
        assert!(out.len() <= 60, "truncated form must be small");
        // Sanity check: the truncation produces "ZZZ...ZZZ..." with 47
        // Z's followed by "...".
        assert!(out.starts_with("ZZ"));
        assert!(out.ends_with("..."));
    }

    #[test]
    fn prompt_singular_sentinel_for_one_extra_assistant_message() {
        let total = MAX_ASSISTANT_MSGS_HEAD + MAX_ASSISTANT_MSGS_TAIL + 1;
        let mut cap = empty_capture();
        for i in 0..total {
            cap.assistant_messages.push(format!("MSG_{i}_body"));
        }
        let out = build_user_prompt("p", "f", &cap);
        assert!(
            out.contains("1 more turn truncated"),
            "singular form for exactly 1 turn truncated, got prompt tail:\n{}",
            out.lines().rev().take(20).collect::<Vec<_>>().join("\n")
        );
        assert!(
            !out.contains("1 more turns truncated"),
            "must not pluralize when count == 1"
        );
    }

    #[test]
    fn prompt_no_truncation_at_assistant_message_boundary() {
        let total = MAX_ASSISTANT_MSGS_HEAD + MAX_ASSISTANT_MSGS_TAIL;
        let mut cap = empty_capture();
        for i in 0..total {
            cap.assistant_messages.push(format!("MSG_{i}_body"));
        }
        let out = build_user_prompt("p", "f", &cap);
        assert!(
            !out.contains("turns truncated") && !out.contains("turn truncated"),
            "no sentinel expected at HEAD+TAIL boundary, got truncation lines:\n{}",
            out.lines()
                .filter(|l| l.contains("truncated"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert!(out.contains("MSG_0_body"));
        let last = total - 1;
        assert!(out.contains(&format!("MSG_{last}_body")));
    }

    #[test]
    fn prompt_lists_tool_calls_before_assistant_message_within_a_turn() {
        // The smoke-test failure that motivated this layout invariant: a
        // judge reading layout order as temporal order concluded the
        // agent "reported contents before reading the file" because the
        // assistant text appeared above the tool calls. Pin the
        // within-turn ordering: a turn's tool calls must precede its
        // final assistant message in the rendered prompt.
        let mut cap = empty_capture();
        cap.assistant_messages.push("ASSISTANT_TURN_TEXT".into());
        cap.tool_calls.push(RecordedToolCall {
            call_id: "1".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "facts.txt"}),
            output: Some("phase 4".into()),
            is_error: false,
            turn_index: 0,
        });
        let out = build_user_prompt("p", "f", &cap);
        let tool_calls_header = out
            .find("Tool calls (in emit order)")
            .expect("turn must list its tool calls");
        let final_msg_header = out
            .find("Final assistant message")
            .expect("turn must list its final message");
        let msg_text = out
            .find("ASSISTANT_TURN_TEXT")
            .expect("final message text must appear");
        assert!(
            tool_calls_header < final_msg_header && final_msg_header < msg_text,
            "within a turn, tool calls must precede the final message; \
             got tool_calls={tool_calls_header}, final_msg={final_msg_header}, \
             msg_text={msg_text}, prompt:\n{out}"
        );
    }

    #[test]
    fn prompt_attributes_tool_calls_to_their_turns() {
        // Cross-turn chronology pin: each turn's tool calls render under
        // that turn's header, and Turn 1 appears before Turn 2 in the
        // prompt (so the judge cannot infer a turn-2 call preceded
        // turn-1's final message). Without per-turn rendering, all tool
        // calls would group at the top, regardless of which turn they
        // were emitted during.
        let mut cap = empty_capture();
        cap.assistant_messages.push("first turn answer".into());
        cap.assistant_messages.push("second turn answer".into());
        cap.tool_calls.push(RecordedToolCall {
            call_id: "a".into(),
            tool_name: ToolName::Read,
            arguments: json!({"path": "TURN_1_CALL_A.txt"}),
            output: Some("ok".into()),
            is_error: false,
            turn_index: 0,
        });
        cap.tool_calls.push(RecordedToolCall {
            call_id: "b".into(),
            tool_name: ToolName::Bash,
            arguments: json!({"command": "TURN_2_CALL_B"}),
            output: Some("ok".into()),
            is_error: false,
            turn_index: 1,
        });
        let out = build_user_prompt("p", "f", &cap);

        let turn1_header = out.find("Turn 1:").expect("Turn 1 header missing");
        let turn2_header = out.find("Turn 2:").expect("Turn 2 header missing");
        let call_a_pos = out
            .find("TURN_1_CALL_A")
            .expect("turn 1's call must appear");
        let msg1_pos = out
            .find("first turn answer")
            .expect("turn 1's message must appear");
        let call_b_pos = out
            .find("TURN_2_CALL_B")
            .expect("turn 2's call must appear");
        let msg2_pos = out
            .find("second turn answer")
            .expect("turn 2's message must appear");

        assert!(
            turn1_header < call_a_pos
                && call_a_pos < msg1_pos
                && msg1_pos < turn2_header
                && turn2_header < call_b_pos
                && call_b_pos < msg2_pos,
            "events must render in real chronological order: Turn 1 header → call A \
             → message 1 → Turn 2 header → call B → message 2; got positions \
             T1={turn1_header}, A={call_a_pos}, M1={msg1_pos}, T2={turn2_header}, \
             B={call_b_pos}, M2={msg2_pos}\nprompt:\n{out}"
        );
    }

    #[test]
    fn prompt_marks_failed_tool_calls_as_errors() {
        let mut cap = empty_capture();
        cap.tool_calls.push(RecordedToolCall {
            call_id: "1".into(),
            tool_name: ToolName::Bash,
            arguments: json!({"command": "false"}),
            output: Some("exit code 1".into()),
            is_error: true,
            turn_index: 0,
        });
        let out = build_user_prompt("p", "f", &cap);
        assert!(
            out.contains("(error)"),
            "failed tool calls should be flagged: {out}"
        );
    }

    // ── Judge::evaluate orchestration ──

    /// Test helper: destructure the first Judge expectation in `scenario`
    /// and call `judge.evaluate` with the unpacked fields. Mirrors what
    /// `apply_judges` does in production.
    async fn evaluate_first_judge(
        judge: &Judge<'_>,
        scenario: &Scenario,
        captured: &CapturedRun,
    ) -> JudgeOutcome {
        let Expectation::Judge {
            pass_when,
            fail_when,
            judge_model,
        } = &scenario.expectations[0]
        else {
            panic!("test setup error: expectations[0] must be Judge");
        };
        judge
            .evaluate(
                scenario.judge_model.as_deref(),
                pass_when,
                fail_when,
                judge_model.as_deref(),
                captured,
            )
            .await
    }

    #[tokio::test]
    async fn evaluate_returns_failed_when_no_model_anywhere() {
        let backend = MockBackend::new(vec![]); // never called
        let judge = Judge::with_backend(Box::new(backend), None);
        let scenario = scenario_with(None, judge_expectation(None));
        let cap = empty_capture();
        let out = evaluate_first_judge(&judge, &scenario, &cap).await;
        match out.verdict {
            JudgeVerdict::Failed { reason } => {
                assert!(
                    reason.contains("no judge model configured"),
                    "expected fail-loud message, got: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            out.record.model.is_none(),
            "model should be None in record when none was configured"
        );
    }

    #[tokio::test]
    async fn evaluate_uses_cli_override_when_present() {
        let backend = MockBackend::new(vec![ok_response(r#"{"passed": true, "reason": "ok"}"#)]);
        let judge = Judge::with_backend(Box::new(backend), Some("cli/judge"));
        let scenario = scenario_with(Some("scn/judge"), judge_expectation(Some("per-exp/judge")));
        let cap = empty_capture();
        let out = evaluate_first_judge(&judge, &scenario, &cap).await;
        assert!(matches!(out.verdict, JudgeVerdict::Passed));
        assert_eq!(
            out.record.model.as_deref(),
            Some("cli/judge"),
            "CLI override should win the precedence chain"
        );
    }

    #[tokio::test]
    async fn evaluate_falls_through_to_scenario_default() {
        let backend = MockBackend::new(vec![ok_response(r#"{"passed": false, "reason": "nope"}"#)]);
        let judge = Judge::with_backend(Box::new(backend), None);
        let scenario = scenario_with(Some("scn/judge"), judge_expectation(None));
        let cap = empty_capture();
        let out = evaluate_first_judge(&judge, &scenario, &cap).await;
        assert!(matches!(out.verdict, JudgeVerdict::Failed { .. }));
        assert_eq!(out.record.model.as_deref(), Some("scn/judge"));
    }

    // ── JudgeVerdict::into_outcome ──

    #[test]
    fn judge_verdict_into_outcome_passed() {
        // Pin the type-bridging contract: `JudgeVerdict::Passed` must
        // become `Outcome::Passed`, never `Outcome::Skipped` (which would
        // silently turn a behavioral check into a no-op on the report).
        assert!(matches!(
            JudgeVerdict::Passed.into_outcome(),
            Outcome::Passed
        ));
    }

    #[test]
    fn judge_verdict_into_outcome_failed_preserves_reason() {
        let v = JudgeVerdict::Failed {
            reason: "specific evidence".into(),
        };
        match v.into_outcome() {
            Outcome::Failed { reason } => assert_eq!(reason, "specific evidence"),
            other => panic!("Failed must lift to Outcome::Failed, got {other:?}"),
        }
    }

    // ── build_judge_outcome (additional) ──

    #[test]
    fn judge_outcome_failed_with_empty_reason_substitutes_diagnostic() {
        // {"passed": false, "reason": ""} must NOT produce an empty
        // failure reason — the user has to learn _why_ the run failed.
        // Substitute a diagnostic that surfaces the raw response so the
        // judge model's pathological output is visible.
        let r = ok_response(r#"{"passed": false, "reason": ""}"#);
        let out = build_judge_outcome("m", "sys", "user", r);
        match out.verdict {
            JudgeVerdict::Failed { reason } => {
                assert!(
                    reason.contains("empty reason"),
                    "diagnostic must call out the empty-reason case, got: {reason}"
                );
                assert!(
                    reason.contains(r#"{"passed": false, "reason": ""}"#),
                    "diagnostic must include raw response so the judge bug is visible, got: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn judge_outcome_failed_with_whitespace_reason_substitutes_diagnostic() {
        // Same as above for reason: "   \n  " — `trim().is_empty()` should
        // match, so the user doesn't get a "Failed:    " message.
        let r = ok_response("{\"passed\": false, \"reason\": \"   \\n  \"}");
        let out = build_judge_outcome("m", "sys", "user", r);
        match out.verdict {
            JudgeVerdict::Failed { reason } => assert!(
                reason.contains("empty reason"),
                "whitespace-only reason must trigger the same diagnostic, got: {reason}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn judge_outcome_failed_when_reason_is_json_null() {
        // JSON null for `reason` lands in the parse-failure branch because
        // JudgeResponse.reason is non-optional `String`. Pin this so a
        // future "make reason optional" refactor doesn't silently switch
        // to the empty-reason diagnostic with a different message.
        let raw = r#"{"passed": false, "reason": null}"#;
        let r = ok_response(raw);
        let out = build_judge_outcome("m", "sys", "user", r);
        match out.verdict {
            JudgeVerdict::Failed { reason } => {
                assert!(
                    reason.contains("invalid JSON"),
                    "null reason must land in invalid-JSON path, not empty-reason path; got: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // Raw response preserved verbatim for debugging.
        assert_eq!(out.record.raw_response, raw);
    }

    // ── build_user_prompt (additional) ──

    #[test]
    fn prompt_caps_assistant_message_count_with_head_tail_window() {
        // Total = HEAD + TAIL + 5 over-budget messages. Verify (a) the
        // head window shows the opening turns, (b) the tail window shows
        // the final turns, (c) the sentinel reports the dropped count,
        // and (d) a middle turn (between head and tail) does NOT appear.
        // The tail-must-appear assertion is the load-bearing one — the
        // judge typically grades on resolution, which lives in the
        // final turns.
        let total = MAX_ASSISTANT_MSGS_HEAD + MAX_ASSISTANT_MSGS_TAIL + 5;
        let mut cap = empty_capture();
        for i in 0..total {
            cap.assistant_messages.push(format!("MSG_{i}_message_body"));
        }
        let out = build_user_prompt("p", "f", &cap);
        assert!(
            out.contains("5 more turns truncated between Turn"),
            "expected head+tail truncation sentinel; got tail:\n{}",
            out.lines().rev().take(15).collect::<Vec<_>>().join("\n")
        );
        // First message must appear (head window).
        assert!(
            out.contains("MSG_0_message_body"),
            "first message must be in the head window"
        );
        // Last message must appear — the resolution context.
        let last_idx = total - 1;
        assert!(
            out.contains(&format!("MSG_{last_idx}_message_body")),
            "last message must be in the tail window so judges see the resolution"
        );
        // Middle (truncated) message must NOT appear.
        let middle_idx = MAX_ASSISTANT_MSGS_HEAD + 2;
        assert!(
            !out.contains(&format!("MSG_{middle_idx}_message_body")),
            "middle message MSG_{middle_idx} should have been truncated"
        );
    }

    // ── validate_judge_config ──

    fn registry_with(provider_id: &str, model_id: &str) -> ProviderRegistry {
        use std::collections::HashMap;

        use crate::config::{Config, ModelCapabilities, ModelConfig, ProviderConfig};

        let cfg = Config {
            providers: HashMap::from_iter([(
                provider_id.into(),
                ProviderConfig {
                    base_url: "http://example.invalid".into(),
                    api_key_env: None, // keyless — no env var lookup
                    models: HashMap::from_iter([(
                        model_id.into(),
                        ModelConfig {
                            id: model_id.into(),
                            name: model_id.into(),
                            context_window: 100_000,
                            max_output_tokens: None,
                            cost: None,
                            capabilities: ModelCapabilities::default(),
                        },
                    )]),
                },
            )]),
            ..Config::default()
        };
        ProviderRegistry::from_config(&cfg).0
    }

    #[test]
    fn validate_passes_when_no_judge_expectations() {
        // Rule-based scenarios should never see judge config errors —
        // the entire validation is a no-op when there are no Judge entries.
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: vec![Expectation::ToolCalled {
                tool: "read".into(),
            }],
            judge_model: None,
        };
        let registry = registry_with("p", "m"); // judge model unused
        validate_judge_config(&scenario, &registry, None).expect("rule-only scenarios pass");
    }

    #[test]
    fn validate_fails_when_no_model_anywhere() {
        let scenario = scenario_with(None, judge_expectation(None));
        let registry = registry_with("p", "m");
        // Use alternate-format ({:#}) per project policy: even though the
        // current error has no nested context, a future `with_context`
        // upstream would silently mask the inner cause from a `to_string`
        // assertion.
        let err = format!(
            "{:#}",
            validate_judge_config(&scenario, &registry, None).expect_err("missing model must bail")
        );
        assert!(
            err.contains("no model configured"),
            "error must mention missing model config, got: {err}"
        );
        // The 1-based index lets users find the offending expectation
        // in their TOML.
        assert!(
            err.contains("expectation #1"),
            "error must include 1-based expectation index, got: {err}"
        );
        // Singular pluralization — exactly one issue should not say "issues".
        assert!(
            err.contains("(1 issue)"),
            "error header should be singular for exactly one failure, got: {err}"
        );
    }

    #[test]
    fn validate_passes_when_cli_model_resolves() {
        let scenario = scenario_with(None, judge_expectation(None));
        let registry = registry_with("fuel-ix", "claude-haiku");
        validate_judge_config(&scenario, &registry, Some("fuel-ix/claude-haiku"))
            .expect("CLI override that resolves should pass");
    }

    #[test]
    fn validate_fails_when_model_not_resolvable() {
        // "Typo a provider name" is the canonical case here — the user
        // wrote `fuel-ixx/...` and we should catch it before running the
        // agent rather than after.
        let scenario = scenario_with(None, judge_expectation(None));
        let registry = registry_with("fuel-ix", "claude-haiku");
        let err = format!(
            "{:#}",
            validate_judge_config(&scenario, &registry, Some("fuel-ixx/claude-haiku"))
                .expect_err("unresolvable model must bail")
        );
        assert!(
            err.contains("not resolvable"),
            "error must mention unresolvable model, got: {err}"
        );
        // Anyhow chain: the inner cause from registry.resolve_model must
        // surface via {err:#}. Assert both the typo'd provider name AND
        // the "not configured" inner-cause phrase so a regression to
        // outer-context-only formatting fails loudly. The previous `||`
        // version masked that regression because either substring alone
        // was enough.
        assert!(
            err.contains("fuel-ixx"),
            "anyhow chain should preserve the offending model ref, got: {err}"
        );
        assert!(
            err.contains("not configured"),
            "alternate-format anyhow should surface the inner registry cause, got: {err}"
        );
    }

    #[test]
    fn validate_uses_resolution_chain_per_expectation() {
        // First expectation uses scenario-level default; second uses its
        // own override. Both must resolve, despite neither having a CLI
        // override.
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: vec![
                judge_expectation(None),          // -> scenario default
                judge_expectation(Some("p2/m2")), // -> per-expectation
            ],
            judge_model: Some("p1/m1".into()),
        };
        let cfg = {
            use std::collections::HashMap;

            use crate::config::{Config, ModelCapabilities, ModelConfig, ProviderConfig};

            Config {
                providers: HashMap::from_iter([
                    (
                        "p1".into(),
                        ProviderConfig {
                            base_url: "http://example.invalid".into(),
                            api_key_env: None,
                            models: HashMap::from_iter([(
                                "m1".into(),
                                ModelConfig {
                                    id: "m1".into(),
                                    name: "m1".into(),
                                    context_window: 100_000,
                                    max_output_tokens: None,
                                    cost: None,
                                    capabilities: ModelCapabilities::default(),
                                },
                            )]),
                        },
                    ),
                    (
                        "p2".into(),
                        ProviderConfig {
                            base_url: "http://example.invalid".into(),
                            api_key_env: None,
                            models: HashMap::from_iter([(
                                "m2".into(),
                                ModelConfig {
                                    id: "m2".into(),
                                    name: "m2".into(),
                                    context_window: 100_000,
                                    max_output_tokens: None,
                                    cost: None,
                                    capabilities: ModelCapabilities::default(),
                                },
                            )]),
                        },
                    ),
                ]),
                ..Config::default()
            }
        };
        let registry = ProviderRegistry::from_config(&cfg).0;
        validate_judge_config(&scenario, &registry, None)
            .expect("both fall-throughs should resolve");
    }

    #[test]
    fn validate_reports_every_failing_judge_at_once() {
        // Two Judge expectations both fail to resolve — the user should see
        // both indices in the error, not just the first. Otherwise they fix
        // #1, re-run, burn the agent budget, then hit #2.
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: vec![
                judge_expectation(None),                 // missing model
                judge_expectation(Some("typo/missing")), // unresolvable model
            ],
            judge_model: None,
        };
        let registry = registry_with("real", "model");
        let err = format!(
            "{:#}",
            validate_judge_config(&scenario, &registry, None)
                .expect_err("both judge expectations are misconfigured")
        );
        assert!(
            err.contains("expectation #1") && err.contains("no model configured"),
            "first failure must surface, got: {err}"
        );
        assert!(
            err.contains("expectation #2") && err.contains("not resolvable"),
            "second failure must also surface, got: {err}"
        );
        assert!(
            err.contains("2 issues"),
            "header should pluralize correctly with 2 failures, got: {err}"
        );
    }

    #[test]
    fn validate_skips_passing_judges_when_reporting_failures() {
        // Three Judge expectations: #1 fails (no model), #2 resolves cleanly
        // via per-expectation override, #3 fails (typo'd model). The error
        // must name #1 and #3 with their original 1-based indices and must
        // NOT name #2 — index drift here would mislead users to edit the
        // wrong line of their TOML.
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: vec![
                judge_expectation(None),                 // #1: missing
                judge_expectation(Some("real/model")),   // #2: resolves
                judge_expectation(Some("typo/missing")), // #3: unresolvable
            ],
            judge_model: None,
        };
        let registry = registry_with("real", "model");
        let err = format!(
            "{:#}",
            validate_judge_config(&scenario, &registry, None)
                .expect_err("expectations #1 and #3 must fail")
        );
        assert!(err.contains("expectation #1"), "got: {err}");
        assert!(err.contains("expectation #3"), "got: {err}");
        assert!(
            !err.contains("expectation #2"),
            "passing judge must not appear in failure list, got: {err}"
        );
        assert!(err.contains("2 issues"), "got: {err}");
    }

    fn scenario_with_n_unconfigured_judges(n: usize) -> Scenario {
        Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: (0..n).map(|_| judge_expectation(None)).collect(),
            judge_model: None,
        }
    }

    #[test]
    fn validate_truncates_long_failure_lists_with_plural_sentinel() {
        // 12 unconfigured judges: 10 surface verbatim, 2 collapse into the
        // sentinel. Header reports the FULL count (12), and adds a
        // "showing first 10" hint so the user knows the listing was capped.
        let scenario = scenario_with_n_unconfigured_judges(12);
        let registry = registry_with("p", "m");
        let err = format!(
            "{:#}",
            validate_judge_config(&scenario, &registry, None)
                .expect_err("12 missing-model judges must bail")
        );
        assert!(
            err.contains("(12 issues, showing first 10)"),
            "header must show full count + first-N hint, got: {err}"
        );
        assert!(
            err.contains("2 more issues truncated"),
            "plural sentinel must report dropped count, got: {err}"
        );
        // FIFO: the FIRST expectation must survive (regression vector if
        // the truncation ever switched to .skip() or split_off, dropping
        // the head instead of the tail). Pin both endpoints of the kept
        // window so neither boundary can silently flip.
        assert!(
            err.contains("expectation #1"),
            "first failure must be preserved by FIFO take(N), got: {err}"
        );
        assert!(err.contains("expectation #10"), "got: {err}");
        assert!(
            !err.contains("expectation #11") && !err.contains("expectation #12"),
            "truncated expectations must not appear verbatim, got: {err}"
        );
    }

    #[test]
    fn validate_truncation_uses_singular_sentinel_for_one_extra() {
        // 11 unconfigured judges: exactly 1 over the cap. The sentinel
        // must use "1 more issue" (singular) — a regression to the plural
        // form would print "1 more issues" which reads broken.
        let scenario = scenario_with_n_unconfigured_judges(11);
        let registry = registry_with("p", "m");
        let err = format!(
            "{:#}",
            validate_judge_config(&scenario, &registry, None)
                .expect_err("11 missing-model judges must bail")
        );
        assert!(
            err.contains("1 more issue truncated"),
            "singular sentinel for exactly 1 truncated, got: {err}"
        );
        assert!(
            !err.contains("1 more issues truncated"),
            "must not pluralize when count == 1, got: {err}"
        );
        // FIFO: the boundary item just above the cap (#11) is the one
        // truncated; #1 must survive.
        assert!(
            err.contains("expectation #1"),
            "first failure must be preserved by FIFO take(N), got: {err}"
        );
        assert!(
            !err.contains("expectation #11"),
            "the over-cap boundary expectation must be in the truncated tail, got: {err}"
        );
    }

    // ── compile-time Send + Sync guards ──

    #[test]
    fn registry_backend_and_judge_are_send_sync() {
        // `JudgeBackend: Send + Sync` is required for spawning judges from
        // tokio tasks. The chain is: RegistryBackend wraps &ProviderRegistry
        // which holds an async-openai Client. If async-openai ever drops
        // Sync from Client across a major version (it's a transitive
        // guarantee, not a contract), this test fails immediately with a
        // clear pointer instead of a downstream impl breaking.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ProviderRegistry>();
        assert_send_sync::<RegistryBackend<'_>>();
        assert_send_sync::<Judge<'_>>();
    }

    // ── apply_judges ──

    #[tokio::test]
    async fn apply_judges_replaces_skipped_with_judge_outcome() {
        let backend = MockBackend::new(vec![ok_response(r#"{"passed": true, "reason": "great"}"#)]);
        let judge = Judge::with_backend(Box::new(backend), Some("cli/judge"));
        let scenario = scenario_with(None, judge_expectation(None));
        let cap = empty_capture();

        let mut report = EvalReport {
            results: vec![
                ExpectationResult {
                    expectation: Expectation::ToolCalled {
                        tool: "read".into(),
                    },
                    outcome: Outcome::Passed,
                    judge: None,
                },
                ExpectationResult {
                    expectation: scenario.expectations[0].clone(),
                    outcome: Outcome::Skipped {
                        reason: "Phase 4 (LLM-as-judge) not yet implemented".into(),
                    },
                    judge: None,
                },
            ],
        };

        apply_judges(&mut report, &scenario, &cap, &judge).await;

        // Non-judge result untouched.
        assert!(
            matches!(report.results[0].outcome, Outcome::Passed),
            "non-judge result must be left alone"
        );
        assert!(report.results[0].judge.is_none());

        // Judge result flipped to Passed and judge metadata populated.
        assert!(matches!(report.results[1].outcome, Outcome::Passed));
        let record = report.results[1]
            .judge
            .as_ref()
            .expect("judge record must be set after apply_judges");
        assert_eq!(record.model.as_deref(), Some("cli/judge"));
        assert!(record.user_prompt.contains("PASS_WHEN"));
        assert!(record.usage.is_some());
    }

    #[tokio::test]
    async fn apply_judges_handles_multiple_judges_independently_in_order() {
        // Two Judge expectations in the report; the mock backend returns a
        // Passed response then a Failed response in queue order. After
        // apply_judges, each expectation must hold the verdict that matches
        // its position — not the reverse (which is what a `Vec::pop()`
        // LIFO mock would silently produce).
        let backend = MockBackend::new(vec![
            ok_response(r#"{"passed": true, "reason": "first one good"}"#),
            ok_response(r#"{"passed": false, "reason": "second one bad"}"#),
        ]);
        let judge = Judge::with_backend(Box::new(backend), Some("cli/judge"));

        let exp_a = Expectation::Judge {
            pass_when: "first criterion".into(),
            fail_when: "first anti".into(),
            judge_model: None,
        };
        let exp_b = Expectation::Judge {
            pass_when: "second criterion".into(),
            fail_when: "second anti".into(),
            judge_model: None,
        };
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: vec![exp_a.clone(), exp_b.clone()],
            judge_model: None,
        };
        let cap = empty_capture();

        let mut report = EvalReport {
            results: vec![
                ExpectationResult {
                    expectation: exp_a,
                    outcome: Outcome::Skipped {
                        reason: "phase 4".into(),
                    },
                    judge: None,
                },
                ExpectationResult {
                    expectation: exp_b,
                    outcome: Outcome::Skipped {
                        reason: "phase 4".into(),
                    },
                    judge: None,
                },
            ],
        };

        apply_judges(&mut report, &scenario, &cap, &judge).await;

        assert!(
            matches!(report.results[0].outcome, Outcome::Passed),
            "FIFO: first queued response (passed=true) must land on first Judge"
        );
        match &report.results[1].outcome {
            Outcome::Failed { reason } => assert_eq!(reason, "second one bad"),
            other => panic!("second Judge should have failed verdict, got {other:?}"),
        }
        // Each result must hold its own user_prompt — not a shared one.
        let p0 = &report.results[0].judge.as_ref().unwrap().user_prompt;
        let p1 = &report.results[1].judge.as_ref().unwrap().user_prompt;
        assert!(p0.contains("first criterion"));
        assert!(p1.contains("second criterion"));
        assert_ne!(p0, p1, "judges must see distinct prompts per expectation");
    }

    #[tokio::test]
    async fn apply_judges_is_noop_when_no_judge_expectations() {
        // A rule-only report should pass through `apply_judges` unchanged.
        // The empty `MockBackend` would panic if `complete` were called, so
        // this test fails loudly on any regression that mistakenly drives
        // the backend for non-Judge variants.
        let backend = MockBackend::new(vec![]);
        let judge = Judge::with_backend(Box::new(backend), Some("cli/judge"));
        let scenario = Scenario {
            name: "x".into(),
            description: "x".into(),
            runs: std::num::NonZeroUsize::new(1).unwrap(),
            setup: Setup::default(),
            user_turns: vec!["go".into()],
            expectations: vec![Expectation::ToolCalled {
                tool: "read".into(),
            }],
            judge_model: None,
        };
        let cap = empty_capture();

        let mut report = EvalReport {
            results: vec![ExpectationResult {
                expectation: Expectation::ToolCalled {
                    tool: "read".into(),
                },
                outcome: Outcome::Passed,
                judge: None,
            }],
        };

        apply_judges(&mut report, &scenario, &cap, &judge).await;

        assert!(matches!(report.results[0].outcome, Outcome::Passed));
        assert!(
            report.results[0].judge.is_none(),
            "rule-only result must not gain a judge field"
        );
    }

    #[tokio::test]
    async fn evaluate_uses_per_expectation_override_when_only_source() {
        // The Phase 1 schema's per-expectation `judge_model` is the
        // narrowest precedence tier; existing evaluate_* tests cover the
        // CLI-only and scenario-fallthrough cases but not this one. Pin
        // it so an argument-order mistake in `Judge::evaluate`'s call to
        // `resolve_judge_model` doesn't slip through.
        let backend = MockBackend::new(vec![ok_response(r#"{"passed": true, "reason": "ok"}"#)]);
        let judge = Judge::with_backend(Box::new(backend), None);
        let scenario = scenario_with(None, judge_expectation(Some("per-exp/judge")));
        let cap = empty_capture();
        let out = evaluate_first_judge(&judge, &scenario, &cap).await;
        assert!(matches!(out.verdict, JudgeVerdict::Passed));
        assert_eq!(
            out.record.model.as_deref(),
            Some("per-exp/judge"),
            "per-expectation override must win when CLI and scenario are both None"
        );
    }
}
