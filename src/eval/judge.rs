//! Paired-comparison LLM judge.
//!
//! The rule-based evaluator handles structural facts (tool sequence,
//! file diffs); behavioral comparisons against a frozen baseline run
//! through this module's [`Judge::compare`] entry point. Per-axis verdicts
//! (correctness, efficiency, etc.) come back as a [`CompareVerdict`] and
//! roll up into the report's paired-comparison column.
//!
//! Architecture: a small [`JudgeBackend`] trait is the test seam — the
//! production [`RegistryBackend`] talks to a provider via
//! [`crate::provider::ProviderRegistry`], while the unit tests in this file
//! use a `MockBackend` returning canned `(text, usage)` pairs or transport
//! errors.
//!
//! A/B order is randomized per call (`rand::random::<bool>()`) to
//! neutralize position bias. Verdict letters in the LLM-facing prompt
//! schema are `a | b | tie`; the caller maps `a`/`b` back to
//! `Verdict::CurrentWins` / `BaselineWins` based on the per-call swap
//! flag. Tests that need deterministic ordering call
//! [`Judge::compare_with_swap`] directly.

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
        score::{Axis, CompareVerdict, PairedScore, Verdict},
        transcript::{NormalizedTranscript, TranscriptEvent},
    },
    event::StreamUsage,
    provider::ProviderRegistry,
    truncate_chars,
};

/// System prompt for `Judge::compare`. Three halo-mitigation invariants
/// are encoded here:
///
/// 1. **Per-axis-rationale-before-verdict** — the LLM emits its rationale
///    *before* the verdict on each axis. Because LLM output is generated
///    left-to-right, this forces per-axis reasoning to be committed to
///    text before any winner is named. Verdict-first would let the model
///    anchor on a winner and rationalize backward — defeating chain-of-
///    thought. The parser is strict about this order; reversed responses
///    are rejected.
///
/// 2. **Tie is first-class** — repeated explicitly so the model treats
///    "roughly equivalent" as the right call rather than forcing a
///    coin-flip between A and B.
///
/// 3. **A/B order randomization** — the user prompt uses neutral
///    "Transcript A" / "Transcript B" labels (the Rust caller swaps
///    which is current vs baseline per call); verdict letters in the
///    LLM-facing prompt schema are `a | b | tie`, never the
///    internal `current_wins | baseline_wins` strings (which DO
///    exist as `Verdict::Display` output but are deliberately kept
///    off the wire — the LLM never sees them). The Rust caller maps
///    `a`/`b` back to `Verdict::CurrentWins` / `BaselineWins` based
///    on the per-call swap flag.
const COMPARE_SYSTEM_PROMPT: &str = "\
You are an evaluator comparing two transcripts of an AI coding agent's \
behavior on the same task. You will be given:

1. The user turns the agent was responding to.
2. The list of axes to score on (with definitions).
3. Transcript A and Transcript B (chronological event sequences).

For each axis, in the order requested, output a YAML mapping with TWO \
keys IN THIS ORDER:

  rationale: <one or two sentences citing specific evidence from both \
             transcripts (turn numbers, tool names, message excerpts) \
             that support the chosen verdict>
  verdict:   a | b | tie

Halo-mitigation rules — follow these even if one transcript looks \
obviously better overall:

- Score each axis INDEPENDENTLY. Do not let a strong showing on one \
  axis bias your judgment on another.
- Emit the `rationale` BEFORE the `verdict` on each axis. Reversed \
  order will be rejected as malformed.
- If both transcripts are roughly equivalent on an axis, return `tie`. \
  Tie is a first-class verdict, NOT a fallback.

Output ONLY the YAML mapping with one top-level key per axis (in the \
requested order), no preamble, no markdown fences, no trailing \
commentary. Example shape:

correctness:
  rationale: \"...\"
  verdict: a
efficiency:
  rationale: \"...\"
  verdict: tie";

const MAX_ASSISTANT_MSG_CHARS: usize = 4096;
const MAX_TOOL_ARGS_CHARS: usize = 1024;
const MAX_TOOL_OUTPUT_CHARS: usize = 2048;
const MAX_RAW_RESPONSE_IN_REASON: usize = 500;
// ──────────────────────────────────────────────────────────────────────
// Backend trait — the test seam.
// ──────────────────────────────────────────────────────────────────────

/// Boundary between the judge orchestration and the actual chat provider
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

/// The two transcripts a `Judge::compare` call evaluates against each
/// other. Bundled because they're conceptually a single thing — "the
/// pair being compared" — and because keeping them separate args from
/// `axes` and `user_turns` would push `compare`'s signature over the
/// clippy `too_many_arguments` threshold without earning a clearer
/// API.
#[derive(Debug, Clone, Copy)]
pub struct ComparePair<'a> {
    pub baseline: &'a NormalizedTranscript,
    pub current: &'a NormalizedTranscript,
}

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

    /// Paired-compare two transcripts on the given axes. Returns
    /// `anyhow::Result<CompareVerdict>` per spec: infrastructure
    /// failures (transport, parser strictness, no judge model) are
    /// `Err`; per-axis judge opinions are inside the `Ok` variant.
    ///
    /// `axes` is the ordered list of dimensions to grade on (typically
    /// `scenario.scoring_axes()` from `Scenario::scoring_axes`, which
    /// returns either the per-scenario override or `DEFAULT_AXES`).
    /// `user_turns` is the scenario's input prompts; both transcripts
    /// were responding to these (passed separately rather than read
    /// from either transcript because they're scenario-level data).
    ///
    /// A/B order is randomized per call (`rand::random::<bool>()`) to
    /// neutralize position bias. The randomization is opaque at this
    /// API level — call `compare_with_swap` directly only from tests
    /// that need deterministic A/B ordering.
    ///
    /// `pair` bundles the baseline and current transcripts (see
    /// `ComparePair`). `scenario_judge_model` is the per-scenario
    /// `judge_model` field from `scenario.toml`, used as a fallback
    /// when no CLI `--judge-model` is set. `compare` runs once per
    /// (scenario, run).
    pub async fn compare(
        &self,
        pair: ComparePair<'_>,
        axes: &[Axis],
        user_turns: &[String],
        scenario_judge_model: Option<&str>,
    ) -> Result<CompareVerdict> {
        let swap = rand::random::<bool>();
        self.compare_with_swap(pair, axes, user_turns, swap, scenario_judge_model)
            .await
    }

    /// Test-and-internal entry point with explicit `swap` control.
    /// Tests call this directly; `compare` is the production wrapper
    /// that chooses `swap` randomly.
    pub(crate) async fn compare_with_swap(
        &self,
        pair: ComparePair<'_>,
        axes: &[Axis],
        user_turns: &[String],
        swap: bool,
        scenario_judge_model: Option<&str>,
    ) -> Result<CompareVerdict> {
        // Empty `axes` would build a prompt with no axes; an empty
        // judge response would then return Ok(vec![]) — a silent
        // no-op masking a caller bug. Bail early before burning a
        // judge call. Caller should always pass at least one axis
        // (typically `scenario.scoring_axes()`, which returns
        // `DEFAULT_AXES` as a non-empty fallback).
        if axes.is_empty() {
            anyhow::bail!(
                "compare: `axes` must be non-empty (got empty slice); pass `DEFAULT_AXES` \
                 or `scenario.scoring_axes()`"
            );
        }
        // Compare resolves CLI > scenario.
        let model = resolve_judge_model(self.cli_model.as_deref(), scenario_judge_model)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no judge model configured: pass --judge-model on the CLI, \
                     or set judge_model on the scenario"
                )
            })?
            .to_string();

        let user_prompt =
            build_compare_user_prompt(pair.baseline, pair.current, axes, user_turns, swap);
        let (raw, _usage) = self
            .backend
            .complete(&model, COMPARE_SYSTEM_PROMPT, &user_prompt)
            .await
            .with_context(|| format!("compare judge call failed (model: {model})"))?;

        if raw.trim().is_empty() {
            anyhow::bail!("compare judge returned empty response (model: {model})");
        }
        parse_compare_response(&raw, axes, swap)
    }
}

/// Adapter trait abstracting `Judge::compare` for testing.
/// `Report::build_from_results` accepts `&dyn JudgeAdapter` so unit
/// tests can substitute fakes that return canned `CompareVerdict`s
/// or simulate transient errors. Production code uses the auto-
/// derived `impl JudgeAdapter for Judge` below.
#[async_trait]
pub trait JudgeAdapter: Send + Sync {
    async fn compare(
        &self,
        pair: ComparePair<'_>,
        axes: &[Axis],
        user_turns: &[String],
        scenario_judge_model: Option<&str>,
    ) -> Result<CompareVerdict>;
}

#[async_trait]
impl<'a> JudgeAdapter for Judge<'a> {
    async fn compare(
        &self,
        pair: ComparePair<'_>,
        axes: &[Axis],
        user_turns: &[String],
        scenario_judge_model: Option<&str>,
    ) -> Result<CompareVerdict> {
        Judge::compare(self, pair, axes, user_turns, scenario_judge_model).await
    }
}

// ──────────────────────────────────────────────────────────────────────
// Pure helpers — fully unit-testable.
// ──────────────────────────────────────────────────────────────────────

/// CLI > scenario default. Returns `None` if no source is set, signaling
/// the caller to emit the "no judge model configured" failure.
pub(crate) fn resolve_judge_model<'a>(
    cli_override: Option<&'a str>,
    scenario_default: Option<&'a str>,
) -> Option<&'a str> {
    cli_override.or(scenario_default)
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
    // After `\`\`\``, accept any optional language tag (json, yaml, etc.)
    // up to the next newline. Without this, `\`\`\`yaml\n...` would leave
    // the `yaml` tag as content, breaking downstream YAML parsing.
    let body = if let Some(after) = trimmed.strip_prefix("```") {
        match after.find('\n') {
            Some(nl) => &after[nl..],
            None => after,
        }
    } else {
        trimmed
    };
    body.trim_start_matches('\n')
        .strip_suffix("```")
        .map(|b| b.trim_end_matches('\n'))
        .unwrap_or(body)
        .trim()
}

/// Render the user prompt for a paired-comparison judge call.
///
/// The `swap` flag determines which transcript is shown as "Transcript A":
/// `swap=false` → A=baseline, B=current; `swap=true` → A=current, B=baseline.
/// The flag is set by the caller via `rand::random::<bool>()` (or a
/// deterministic value in tests). The LLM never sees the original labels;
/// the verdict letters it emits (`a | b | tie`) are mapped back to
/// `Verdict::CurrentWins` / `BaselineWins` by `parse_compare_response`,
/// taking `swap` into account.
pub(crate) fn build_compare_user_prompt(
    baseline: &NormalizedTranscript,
    current: &NormalizedTranscript,
    axes: &[Axis],
    user_turns: &[String],
    swap: bool,
) -> String {
    let mut out = String::new();

    out.push_str("USER TURNS — the prompts both transcripts were responding to:\n");
    if user_turns.is_empty() {
        out.push_str("  (no user turns recorded)\n");
    } else {
        for (i, turn) in user_turns.iter().enumerate() {
            out.push_str(&format!("  {}. {}\n", i + 1, turn));
        }
    }
    out.push('\n');

    out.push_str("AXES TO SCORE — output one YAML block per axis, in this order:\n");
    for axis in axes {
        out.push_str(&format!("  - {axis}: {}\n", axis.definition()));
    }
    out.push('\n');

    let (a, b) = if swap {
        (current, baseline)
    } else {
        (baseline, current)
    };
    out.push_str("Transcript A — events in chronological order:\n");
    render_transcript_events(a, &mut out);
    out.push('\n');
    out.push_str("Transcript B — events in chronological order:\n");
    render_transcript_events(b, &mut out);
    out.push('\n');

    out.push_str(
        "Now produce the YAML mapping. Remember: rationale BEFORE verdict on each \
         axis; tie is allowed; output ONLY the mapping with no preamble.\n",
    );

    out
}

/// Render a `NormalizedTranscript`'s event sequence into the user prompt.
/// Trims long tool outputs and assistant messages using the `MAX_TOOL_*` /
/// `MAX_ASSISTANT_MSG_CHARS` caps so paired-comparison prompts stay
/// token-bounded.
fn render_transcript_events(t: &NormalizedTranscript, out: &mut String) {
    if t.events.is_empty() {
        out.push_str("  (no events)\n");
        return;
    }
    for (i, event) in t.events.iter().enumerate() {
        match event {
            TranscriptEvent::ToolCall {
                tool_name,
                arguments,
            } => {
                let mut compact = String::new();
                format_args_compact(arguments, MAX_TOOL_ARGS_CHARS, &mut compact);
                let args = truncate_chars(&compact, MAX_TOOL_ARGS_CHARS);
                out.push_str(&format!(
                    "  {}. tool_call {}({})\n",
                    i + 1,
                    tool_name.as_str(),
                    args
                ));
            }
            TranscriptEvent::ToolResult {
                tool_name,
                output,
                is_error,
            } => {
                let prefix = if *is_error { "(error) " } else { "" };
                let body = truncate_chars(output, MAX_TOOL_OUTPUT_CHARS);
                out.push_str(&format!(
                    "  {}. tool_result {} -> {}{}\n",
                    i + 1,
                    tool_name.as_str(),
                    prefix,
                    body
                ));
            }
            TranscriptEvent::AssistantMessage { text } => {
                let body = truncate_chars(text, MAX_ASSISTANT_MSG_CHARS);
                let indented = body.replace('\n', "\n      ");
                out.push_str(&format!(
                    "  {}. assistant_message:\n      {indented}\n",
                    i + 1
                ));
            }
        }
    }
}

/// Strict parser for the YAML response from `Judge::compare`.
///
/// Five layers of validation, in execution order:
///
/// 1. **Markdown fence strip**: `strip_markdown_fences` removes
///    optional triple-backtick wrappers (with any language tag).
///
/// 2. **Block-scalar pre-check**: a `rationale: |` or `verdict: |`
///    (likewise `>`) block scalar's body lines can contain text
///    that looks like a YAML key, fooling the positional check
///    in step 4. Reject up front.
///
/// 3. **Schema deserialization** via `serde-saphyr` into
///    `BTreeMap<String, RawAxisResponse>`. Unknown verdict letters
///    fail to deserialize via the closed `RawVerdict` enum. An
///    empty mapping (judge returned `null`, `{}`, or whitespace)
///    is caught here with a clear diagnostic.
///
/// 4. **Multi-line rationale rejection**: any parsed rationale
///    containing a newline indicates a multi-line quoted scalar
///    in the source — its continuation lines can contain text
///    that looks like an axis key and corrupt the positional
///    boundary detection in step 5. Reject (the prompt asks for
///    single-line quoted strings).
///
/// 5. **Positional order check** on the raw text, gated by the
///    parsed key set from step 3. For each parsed axis, the key
///    must be locatable at line-start, and within its region,
///    `rationale:` must precede `verdict:`. This is the load-
///    bearing halo-mitigation invariant.
///
/// 6. **Per-axis assembly + coverage check**: every requested axis
///    must appear with a non-empty rationale; the verdict letter
///    is translated to `Verdict::{CurrentWins, BaselineWins, Tie}`
///    based on the per-call `swap` flag. No extra unrequested axes
///    may appear.
///
/// Returns scores in the requested-axes order (NOT the LLM's emit
/// order).
pub(crate) fn parse_compare_response(
    raw: &str,
    requested_axes: &[Axis],
    swap: bool,
) -> Result<CompareVerdict> {
    // Step 1: markdown fence strip.
    let stripped = strip_markdown_fences(raw);

    // Step 2: block-scalar pre-check. A `rationale: |` or
    // `verdict: |` (likewise `>`) block scalar's body lines can
    // contain text that looks like a YAML key — `efficiency: was
    // poor.` inside a multi-line rationale body would fool the
    // line-anchored region boundaries below and either silently
    // skip the order check on that axis or trigger a spurious bail
    // on the next. The prompt asks for quoted strings; reject up
    // front.
    if let Some(field) = detect_block_scalar_value(stripped) {
        anyhow::bail!(
            "compare response uses a YAML block scalar (`|` or `>`) for the `{field}` \
             value; the prompt requires quoted-string values so the positional \
             halo-mitigation check can verify key order"
        );
    }

    // Step 3: schema deserialization. Runs before the order check
    // so the latter can use the parsed key set as ground truth —
    // that's how flow-style YAML gets caught (the positional check
    // needs the LLM's block-style indentation, and the only way to
    // know which axes actually appeared in the response is to
    // parse it).
    let parsed: std::collections::BTreeMap<String, RawAxisResponse> =
        serde_saphyr::from_str(stripped).with_context(|| {
            let snippet = truncate_chars(raw, MAX_RAW_RESPONSE_IN_REASON);
            format!("compare response did not parse as YAML; raw: {snippet}")
        })?;
    if parsed.is_empty() {
        let snippet = truncate_chars(raw, MAX_RAW_RESPONSE_IN_REASON);
        anyhow::bail!(
            "compare response parsed as empty mapping (e.g., the judge returned `null`, `{{}}`, or whitespace); raw: {snippet}"
        );
    }

    // Step 4: multi-line quoted-scalar rejection. YAML folds line
    // breaks in `"..."` and `'...'` scalars to spaces, so checking
    // the PARSED value for newlines doesn't catch a multi-line
    // source. We have to scan the raw source for a `rationale:` /
    // `verdict:` line whose value opens a quote but doesn't close
    // it — the continuation lines can be picked up by step 5's
    // positional check as fake axis keys, corrupting region
    // boundaries. The prompt asks for single-line strings; enforce
    // it on the source layout so step 5 always sees a clean one.
    if let Some(field) = detect_multiline_quoted_value(stripped) {
        anyhow::bail!(
            "compare response uses a multi-line quoted scalar for the `{field}` value; \
             the prompt requires single-line quoted strings (continuation lines inside \
             the value can be misread by the positional halo-mitigation check as fake \
             axis keys)"
        );
    }

    // Step 5: positional order check on the raw text, gated by
    // the parsed key set. Each axis present in the parse must be
    // locatable at line-start; if not, the response is flow-style
    // or wonky indentation we can't verify positionally — reject.
    // This is the load-bearing halo-mitigation invariant from the
    // spec.
    enforce_rationale_before_verdict(stripped, &parsed, requested_axes)?;

    // 3a. Every requested axis present; non-empty rationale;
    //     translate verdict letter.
    let mut out = Vec::with_capacity(requested_axes.len());
    for axis in requested_axes {
        let key = format!("{axis}");
        let raw_resp = parsed
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("compare response missing axis {key:?}"))?;
        if raw_resp.rationale.trim().is_empty() {
            anyhow::bail!(
                "compare response on axis {key:?} has an empty `rationale`; \
                 the prompt requires non-empty reasoning before the verdict \
                 (halo-mitigation)"
            );
        }
        let verdict = match raw_resp.verdict {
            RawVerdict::A => {
                if swap {
                    Verdict::CurrentWins
                } else {
                    Verdict::BaselineWins
                }
            }
            RawVerdict::B => {
                if swap {
                    Verdict::BaselineWins
                } else {
                    Verdict::CurrentWins
                }
            }
            RawVerdict::Tie => Verdict::Tie,
        };
        out.push(PairedScore {
            axis: *axis,
            rationale: raw_resp.rationale.clone(),
            verdict,
        });
    }

    // 3b. No extra unrequested axes.
    let requested_keys: std::collections::BTreeSet<String> =
        requested_axes.iter().map(|a| format!("{a}")).collect();
    for key in parsed.keys() {
        if !requested_keys.contains(key) {
            anyhow::bail!("compare response has unexpected axis key {key:?}");
        }
    }

    Ok(out)
}

/// Schema of one axis's slice of the raw judge response. Field order
/// reflects what the prompt asks the LLM to emit (rationale BEFORE
/// verdict). The serde derive's tolerance of either order is
/// intentional — `enforce_rationale_before_verdict` does the order
/// check separately, on the source text, before this struct sees the
/// data.
#[derive(Debug, Deserialize)]
struct RawAxisResponse {
    rationale: String,
    verdict: RawVerdict,
}

/// Verdict letters as the LLM emits them. `serde(rename_all)` matches
/// the lowercased letters in the prompt schema; an unknown letter
/// fails to deserialize and propagates as `Err` from
/// `parse_compare_response`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawVerdict {
    A,
    B,
    Tie,
}

/// Positional check: for each axis that appears in `parsed`, find its
/// key in `stripped` (line-anchored, tolerant of leading whitespace);
/// then within the slice from that key to the next axis key (or
/// end-of-string), assert the `rationale:` key precedes the
/// `verdict:` key. Returns `Err` naming the first offending axis on
/// violation.
///
/// Three failure shapes:
///
/// 1. Axis present in parse but not locatable as a line-anchored key
///    (e.g., flow-style top-level `{correctness: {...}}`). Bail.
/// 2. Axis IS locatable but its inner `rationale:` / `verdict:` keys
///    aren't — flow-style sub-mapping value (`correctness: {...}`),
///    so the positional halo-mitigation check can't verify order.
///    Bail.
/// 3. Within an axis's region, `verdict:` appears before `rationale:`.
///    Bail.
///
/// Axes in `requested_axes` that are NOT in `parsed` are skipped —
/// the missing-axis case is caught downstream by the coverage check
/// in `parse_compare_response`.
///
/// All searches go through `find_line_anchored_key`, which matches a
/// field name followed by optional whitespace and a `:`. That's
/// load-bearing: a previous version used plain `region.find("verdict")`
/// and was vulnerable to the word appearing inside a rationale string
/// or a YAML comment (caught by first Copilot review pass). The
/// whitespace-before-colon tolerance handles `verdict : a` (which
/// serde-saphyr accepts as valid YAML).
fn enforce_rationale_before_verdict(
    stripped: &str,
    parsed: &std::collections::BTreeMap<String, RawAxisResponse>,
    requested_axes: &[Axis],
) -> Result<()> {
    let keys: Vec<String> = requested_axes.iter().map(|a| format!("{a}")).collect();

    for (i, key) in keys.iter().enumerate() {
        // Missing axes are caught downstream by the coverage check —
        // not our concern here.
        if !parsed.contains_key(key) {
            continue;
        }
        let axis_start = find_line_anchored_key(stripped, key).ok_or_else(|| {
            anyhow::anyhow!(
                "compare response on axis {key:?} is present in the parse but the \
                 key is not at the start of any line (flow-style YAML or unusual \
                 structure); the positional rationale-before-verdict check requires \
                 block-style keys to verify halo-mitigation"
            )
        })?;
        // End of this axis's region: the start of the next axis key,
        // or end-of-string.
        let axis_end = keys[i + 1..]
            .iter()
            .filter_map(|k| find_line_anchored_key(stripped, k))
            .filter(|&p| p > axis_start)
            .min()
            .unwrap_or(stripped.len());
        let region = &stripped[axis_start..axis_end];

        // Inner keys: same line-anchored lookup. If either is missing
        // here, the axis's value is flow-style or otherwise structured
        // so we can't verify positionally — bail rather than silently
        // skip, otherwise a malformed response could evade the check.
        let r_pos = find_line_anchored_key(region, "rationale");
        let v_pos = find_line_anchored_key(region, "verdict");
        match (r_pos, v_pos) {
            (Some(r), Some(v)) if r >= v => {
                anyhow::bail!(
                    "compare response on axis {key:?} has `verdict:` before `rationale:`; \
                     the prompt requires rationale-before-verdict for halo-mitigation"
                );
            }
            (Some(_), Some(_)) => {} // ordered correctly; continue
            _ => {
                anyhow::bail!(
                    "compare response on axis {key:?}: could not locate both \
                     `rationale:` and `verdict:` keys at line-start within the \
                     axis region (flow-style sub-mapping or unusual structure); \
                     the positional halo-mitigation check requires block-style keys"
                );
            }
        }
    }
    Ok(())
}

/// Find the byte offset of `field` when it appears as a YAML mapping
/// key at the start of a line, optionally preceded by ASCII whitespace
/// and optionally separated from its colon by additional whitespace.
/// Matches `<leading-ws><field><optional-ws>:`. Returns the offset of
/// `field` itself (after any leading whitespace), or `None` if no
/// matching line exists.
///
/// Two non-obvious choices baked in:
///
/// 1. The `field` argument is the bare name (no colon). Matching
///    `<field><optional-ws>:` lets the function locate both `verdict:`
///    and `verdict :`, which serde-saphyr accepts equivalently.
/// 2. The match requires `field` to be followed by whitespace-then-
///    colon (not by another identifier character). That rejects
///    longer-identifier prefixes like `correctness_v2` when asked
///    for `correctness` — a false-match there would conflate distinct
///    axes.
///
/// Used by `enforce_rationale_before_verdict` to locate YAML field
/// headers without false-matching on the same word inside a rationale
/// string body or a YAML comment.
fn find_line_anchored_key(haystack: &str, field: &str) -> Option<usize> {
    let mut byte_offset = 0;
    for line in haystack.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(field)
            && rest.trim_start().starts_with(':')
        {
            let leading_ws = line.len() - trimmed.len();
            return Some(byte_offset + leading_ws);
        }
        byte_offset += line.len();
    }
    None
}

/// Scan `stripped` for any line that introduces a YAML block scalar
/// (`|` or `>` with optional chomping indicator) as the value of
/// `rationale` or `verdict`. Returns the field name on the first
/// match, or `None` if no block-scalar values are present.
///
/// Block scalars break the line-anchored positional check because
/// their body lines can contain text that looks like a YAML key (e.g.,
/// `efficiency: was poor.`), fooling axis-region boundary detection.
/// The prompt explicitly asks for quoted-string values; this scanner
/// enforces that the response complies.
fn detect_block_scalar_value(stripped: &str) -> Option<&'static str> {
    for line in stripped.lines() {
        let trimmed = line.trim_start();
        for &field in &["rationale", "verdict"] {
            if let Some(rest) = trimmed.strip_prefix(field)
                && let Some(after_colon) = rest.trim_start().strip_prefix(':')
            {
                let value = after_colon.trim_start();
                if matches!(value.chars().next(), Some('|') | Some('>')) {
                    return Some(field);
                }
            }
        }
    }
    None
}

/// Scan `stripped` for any line that opens a quoted scalar (`"` or
/// `'`) on a `rationale:` or `verdict:` value but does not close it
/// on the same line. Returns the field name on the first match.
///
/// Multi-line quoted scalars in `"..."` and `'...'` are valid YAML,
/// but their continuation lines in the source can begin with text
/// that looks like a YAML key (e.g., `    verdict: bad`), fooling
/// the positional halo-mitigation check downstream. The prompt asks
/// for single-line quoted strings; this scanner enforces it on the
/// source layout (the parsed value can't tell us — YAML folds the
/// line break to a space inside `"..."`).
///
/// The check is intentionally loose: it doesn't track YAML's
/// backslash-escape semantics inside double quotes, so a
/// `rationale: "she said \""` followed by a continuation line could
/// false-negative. That's acceptable — LLMs essentially never emit
/// escaped quotes in rationales, and the downstream positional check
/// still catches the corrupted boundaries (with a less precise but
/// still loud diagnostic).
fn detect_multiline_quoted_value(stripped: &str) -> Option<&'static str> {
    for line in stripped.lines() {
        let trimmed = line.trim_start();
        for &field in &["rationale", "verdict"] {
            if let Some(rest) = trimmed.strip_prefix(field)
                && let Some(after_colon) = rest.trim_start().strip_prefix(':')
            {
                let value = after_colon.trim_start();
                for &quote in &['"', '\''] {
                    if let Some(value_after_open) = value.strip_prefix(quote)
                        && !value_after_open.contains(quote)
                    {
                        return Some(field);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use super::*;

    /// One queued canned response for the mock backend: either a
    /// `(text, usage)` pair or a transport-style error.
    type CannedResponse = Result<(String, Option<StreamUsage>)>;

    /// Test backend: returns canned responses (success or transport error)
    /// in the order they were queued. Uses a `VecDeque` + `pop_front` so
    /// multi-response tests get FIFO order — a `Vec` + `pop` would silently
    /// reverse the queue.
    ///
    /// `model_recorder` is an optional out-channel that captures the
    /// `model_ref` of each `complete` call, so tests can assert that the
    /// orchestrator threaded the right model through. The default
    /// constructor leaves it empty (tests that don't care don't pay for
    /// it); `with_model_recorder` is the opt-in form.
    struct MockBackend {
        responses: Mutex<VecDeque<CannedResponse>>,
        model_recorder: Arc<Mutex<Vec<String>>>,
    }

    impl MockBackend {
        fn new(responses: Vec<CannedResponse>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
                model_recorder: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_model_recorder(
            responses: Vec<CannedResponse>,
            recorder: Arc<Mutex<Vec<String>>>,
        ) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
                model_recorder: recorder,
            }
        }
    }

    #[async_trait]
    impl JudgeBackend for MockBackend {
        async fn complete(
            &self,
            model_ref: &str,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> CannedResponse {
            self.model_recorder
                .lock()
                .expect("mock recorder lock poisoned")
                .push(model_ref.to_string());
            self.responses
                .lock()
                .expect("mock lock poisoned")
                .pop_front()
                .expect("MockBackend out of canned responses")
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

    // ── Pure: resolve_judge_model ──

    #[test]
    fn cli_model_takes_precedence_over_scenario() {
        let m = resolve_judge_model(Some("cli/x"), Some("scn/z"));
        assert_eq!(m, Some("cli/x"));
    }

    #[test]
    fn scenario_default_used_when_no_cli_override() {
        let m = resolve_judge_model(None, Some("scn/z"));
        assert_eq!(m, Some("scn/z"));
    }

    #[test]
    fn no_model_anywhere_returns_none() {
        let m = resolve_judge_model(None, None);
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

    // ── compare prompt + parser + Judge::compare ──

    use crate::eval::transcript::{NormalizedTranscript, TranscriptEvent, UsageSummary};

    fn make_test_transcript(events: Vec<TranscriptEvent>) -> NormalizedTranscript {
        NormalizedTranscript {
            events,
            deterministic_floor_passed: true,
            usage_summary: UsageSummary {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
                duration_ms: 1234,
            },
        }
    }

    #[test]
    fn compare_user_prompt_lists_requested_axes_in_order() {
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let prompt = build_compare_user_prompt(
            &baseline,
            &current,
            &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
            &["go".to_string()],
            false,
        );
        let c = prompt.find("correctness").expect("correctness in prompt");
        let e = prompt.find("efficiency").expect("efficiency in prompt");
        let n = prompt.find("conciseness").expect("conciseness in prompt");
        assert!(c < e && e < n, "axes must be presented in requested order");
    }

    #[test]
    fn compare_user_prompt_swap_flag_swaps_a_and_b_assignment() {
        let baseline = make_test_transcript(vec![TranscriptEvent::AssistantMessage {
            text: "BASELINE_MARKER".into(),
        }]);
        let current = make_test_transcript(vec![TranscriptEvent::AssistantMessage {
            text: "CURRENT_MARKER".into(),
        }]);
        let unswapped =
            build_compare_user_prompt(&baseline, &current, &[Axis::Correctness], &[], false);
        let swapped =
            build_compare_user_prompt(&baseline, &current, &[Axis::Correctness], &[], true);
        let a_pos_un = unswapped.find("Transcript A").unwrap();
        let b_pos_un = unswapped.find("Transcript B").unwrap();
        let baseline_pos_un = unswapped.find("BASELINE_MARKER").unwrap();
        let current_pos_un = unswapped.find("CURRENT_MARKER").unwrap();
        assert!(a_pos_un < baseline_pos_un && baseline_pos_un < b_pos_un);
        assert!(b_pos_un < current_pos_un);
        let a_pos_sw = swapped.find("Transcript A").unwrap();
        let b_pos_sw = swapped.find("Transcript B").unwrap();
        let baseline_pos_sw = swapped.find("BASELINE_MARKER").unwrap();
        let current_pos_sw = swapped.find("CURRENT_MARKER").unwrap();
        assert!(a_pos_sw < current_pos_sw && current_pos_sw < b_pos_sw);
        assert!(b_pos_sw < baseline_pos_sw);
    }

    #[test]
    fn compare_user_prompt_includes_user_turns() {
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let prompt = build_compare_user_prompt(
            &baseline,
            &current,
            &[Axis::Correctness],
            &[
                "READ_THE_README".to_string(),
                "FOLLOW_UP_QUESTION".to_string(),
            ],
            false,
        );
        assert!(prompt.contains("READ_THE_README"));
        assert!(prompt.contains("FOLLOW_UP_QUESTION"));
    }

    #[test]
    fn compare_user_prompt_uses_neutral_a_b_labels_not_baseline_current() {
        // Halo invariant: the LLM must NOT see which transcript is
        // baseline vs current. If "baseline" or "current" leak into
        // the prompt body, position-bias mitigation is defeated.
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let prompt =
            build_compare_user_prompt(&baseline, &current, &[Axis::Correctness], &[], false);
        let a_idx = prompt.find("Transcript A").unwrap();
        let b_idx = prompt.find("Transcript B").unwrap();
        let between = &prompt[a_idx..b_idx];
        assert!(
            !between.to_lowercase().contains("baseline"),
            "transcript section must not label its content 'baseline': {between}"
        );
        assert!(
            !between.to_lowercase().contains("current"),
            "transcript section must not label its content 'current': {between}"
        );
    }

    fn requested_axes() -> Vec<Axis> {
        vec![Axis::Correctness, Axis::Efficiency, Axis::Conciseness]
    }

    #[test]
    fn parse_compare_response_returns_per_axis_verdicts_in_requested_order() {
        let raw = r#"
correctness:
  rationale: "A handles the edge case; B does not."
  verdict: a
efficiency:
  rationale: "A made one fewer tool call."
  verdict: a
conciseness:
  rationale: "Both transcripts were similarly succinct."
  verdict: tie
"#;
        // swap=false → A is baseline, so verdict=a → BaselineWins;
        // verdict=tie → Tie regardless of swap.
        let scores = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].axis, Axis::Correctness);
        assert_eq!(scores[0].verdict, Verdict::BaselineWins);
        assert!(scores[0].rationale.contains("edge case"));
        assert_eq!(scores[1].axis, Axis::Efficiency);
        assert_eq!(scores[1].verdict, Verdict::BaselineWins);
        assert_eq!(scores[2].axis, Axis::Conciseness);
        assert_eq!(scores[2].verdict, Verdict::Tie);
    }

    #[test]
    fn parse_compare_response_translates_verdict_letters_with_swap_flag() {
        let raw = r#"
correctness:
  rationale: "A wins on this axis."
  verdict: a
efficiency:
  rationale: "B is more efficient."
  verdict: b
conciseness:
  rationale: "Tie."
  verdict: tie
"#;
        let scores = parse_compare_response(raw, &requested_axes(), true).unwrap();
        assert_eq!(scores[0].verdict, Verdict::CurrentWins); // a + swap=true
        assert_eq!(scores[1].verdict, Verdict::BaselineWins); // b + swap=true
        assert_eq!(scores[2].verdict, Verdict::Tie);
        // Verify the unswapped translation for sanity:
        let scores2 = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores2[0].verdict, Verdict::BaselineWins);
        assert_eq!(scores2[1].verdict, Verdict::CurrentWins);
        assert_eq!(scores2[2].verdict, Verdict::Tie);
    }

    #[test]
    fn parse_compare_response_rejects_verdict_before_rationale() {
        // Halo-mitigation invariant: the strict order check.
        let raw = r#"
correctness:
  verdict: a
  rationale: "out-of-order"
efficiency:
  rationale: "ok"
  verdict: tie
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rationale") && msg.contains("verdict"),
            "expected error to mention key order; got: {msg}"
        );
        assert!(
            msg.contains("correctness"),
            "expected error to name the offending axis; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_missing_axis() {
        // We asked for 3 axes; LLM only emitted 2.
        let raw = r#"
correctness:
  rationale: "ok"
  verdict: a
efficiency:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("conciseness"),
            "expected error to name the missing axis; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_unknown_verdict_letter() {
        let raw = r#"
correctness:
  rationale: "garbage"
  verdict: maybe
efficiency:
  rationale: "ok"
  verdict: a
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("verdict") || msg.contains("maybe"),
            "expected error to flag the unknown verdict letter; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_extra_unrequested_axis() {
        // LLM hallucinated a "speed" axis we didn't ask for.
        let raw = r#"
correctness:
  rationale: "ok"
  verdict: a
efficiency:
  rationale: "ok"
  verdict: a
conciseness:
  rationale: "ok"
  verdict: a
speed:
  rationale: "ok"
  verdict: a
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("speed") || msg.contains("unexpected"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_strips_markdown_fences() {
        // Some models wrap structured output in ```yaml ... ``` despite
        // the prompt saying not to. The existing strip_markdown_fences
        // helper handles this; reuse it.
        let raw = "```yaml\ncorrectness:\n  rationale: \"ok\"\n  verdict: a\nefficiency:\n  rationale: \"ok\"\n  verdict: a\nconciseness:\n  rationale: \"ok\"\n  verdict: tie\n```";
        let scores = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores.len(), 3);
    }

    #[test]
    fn parse_compare_response_rejects_flow_style_yaml() {
        // Halo-mitigation invariant: the rationale-before-verdict order
        // check is positional on the source text. Flow-style YAML
        // packs the whole mapping onto one line with no `\nkey:`
        // anchors, so naive line-start scanning could miss it.
        // A response that parses successfully but evades the order
        // check would silently defeat halo mitigation — reject.
        let raw = r#"{correctness: {verdict: a, rationale: "r"}, efficiency: {verdict: a, rationale: "r"}, conciseness: {verdict: tie, rationale: "r"}}"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("flow"),
            "expected error to flag flow-style YAML; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_partial_flow_style_value() {
        // Second review found: a top-level block-style axis key with a
        // flow-style sub-mapping value defeats the inner-key positional
        // check — `find_line_anchored_key` can't locate `rationale:` /
        // `verdict:` inside `{rationale: "r", verdict: a}`. Previously
        // the order check silently `if let`-fell-through; downstream
        // produced a misleading "missing axis" error. Bail explicitly.
        let raw = r#"correctness: {verdict: a, rationale: "r"}
efficiency:
  rationale: "ok"
  verdict: tie
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("correctness")
                && (msg.contains("flow") || msg.contains("rationale:") || msg.contains("verdict:")),
            "expected flow-style sub-mapping rejection naming correctness; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_block_scalar_rationale_value() {
        // Second review found: a `rationale: |` block scalar whose body
        // contains a line that looks like a key (e.g., `efficiency:`)
        // fools `find_line_anchored_key` into matching INSIDE the body
        // text — silently shifting axis-region boundaries and producing
        // either silent halo-violation skips or spurious bails on the
        // next axis. Reject block scalars up front; the prompt asks for
        // quoted strings.
        let raw = r#"
correctness:
  rationale: |
    Notes about the run.
    efficiency: was poor.
  verdict: a
efficiency:
  rationale: "ok"
  verdict: tie
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("block scalar") || msg.contains("quoted"),
            "expected block-scalar rejection; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_block_scalar_folded_verdict_value() {
        // Same defense for `>` folded scalars; less common than `|` but
        // YAML accepts both. Verdict block-scalars are nonsensical (the
        // value is `a | b | tie`) but the line-anchoring vulnerability
        // is symmetric.
        let raw = "correctness:\n  rationale: \"ok\"\n  verdict: >\n    a\nefficiency:\n  rationale: \"ok\"\n  verdict: tie\nconciseness:\n  rationale: \"ok\"\n  verdict: tie";
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("block scalar") || msg.contains("quoted"),
            "expected block-scalar rejection; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_multiline_quoted_rationale() {
        // A multi-line quoted scalar parses fine in YAML, but its
        // source-text continuation line can contain content that
        // looks like an axis key (e.g., `    efficiency: was
        // discussed`) and fool `find_line_anchored_key` into
        // corrupting axis-region boundaries. The block-scalar
        // pre-check only covers `|`/`>`, not multi-line `"..."`.
        // Reject parsed rationales containing newlines so this
        // fails with a clear diagnostic before the
        // boundary detection runs.
        let raw = "correctness:\n  rationale: \"first line\n    efficiency: was discussed\"\n  verdict: a\nefficiency:\n  rationale: \"ok\"\n  verdict: tie\nconciseness:\n  rationale: \"ok\"\n  verdict: tie";
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            (msg.contains("multi-line") || msg.contains("multiline")) && msg.contains("rationale"),
            "expected multi-line rationale rejection; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_multiline_rationale_even_when_value_is_innocuous() {
        // Defense in depth: even when the continuation lines don't
        // happen to contain anything that looks like a key, multi-line
        // rationales violate the prompt's single-line-string contract
        // and are rejected. Pinning this contract surface so a future
        // refactor can't relax it for "harmless" multi-line content.
        let raw = "correctness:\n  rationale: \"line one\n    line two\"\n  verdict: a\nefficiency:\n  rationale: \"ok\"\n  verdict: tie\nconciseness:\n  rationale: \"ok\"\n  verdict: tie";
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("multi-line") || msg.contains("multiline") || msg.contains("newline"),
            "expected multi-line rationale rejection; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_catches_verdict_before_rationale_with_space_before_colon() {
        // Second review found: serde-saphyr accepts `verdict : a` (space
        // before colon) as valid YAML. The line-anchored finder uses
        // literal `"verdict:"` and misses keys with a space before the
        // colon. A verdict-before-rationale halo violation in this
        // formatting would silently pass. The fix loosens the finder
        // to match `field` + optional whitespace + `:`.
        let raw = r#"
correctness:
  verdict : a
  rationale : "out of order"
efficiency:
  rationale: "ok"
  verdict: tie
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("verdict") && msg.contains("rationale") && msg.contains("correctness"),
            "expected verdict-before-rationale rejection under space-before-colon; got: {msg}"
        );
    }

    #[test]
    fn find_line_anchored_key_returns_byte_offset_of_key_not_line_start() {
        // The byte-offset is load-bearing: callers slice `stripped`
        // for axis-region boundaries. Off-by-N (N = leading whitespace
        // count) would silently corrupt every boundary downstream.
        let h = "  rationale: foo\n  verdict: bar";
        assert_eq!(find_line_anchored_key(h, "rationale"), Some(2));
        let v_offset = "  rationale: foo\n".len() + 2;
        assert_eq!(find_line_anchored_key(h, "verdict"), Some(v_offset));
    }

    #[test]
    fn find_line_anchored_key_matches_on_final_line_without_trailing_newline() {
        // split_inclusive('\n') vs lines() boundary case — a refactor
        // regressing to .lines() would silently miss the last line's
        // key, weakening the order check on responses that end without
        // a trailing newline (a common LLM emission).
        let h = "correctness:\n  rationale: \"ok\"\n  verdict: a";
        assert!(find_line_anchored_key(h, "verdict").is_some());
    }

    #[test]
    fn find_line_anchored_key_accepts_whitespace_before_colon() {
        // serde-saphyr accepts `verdict : a` as valid YAML. The
        // line-anchored finder must too — otherwise the order check
        // silently skips lines with that formatting.
        let h = "  verdict : a\n  rationale : \"ok\"";
        assert_eq!(find_line_anchored_key(h, "verdict"), Some(2));
        let r_offset = "  verdict : a\n".len() + 2;
        assert_eq!(find_line_anchored_key(h, "rationale"), Some(r_offset));
    }

    #[test]
    fn find_line_anchored_key_does_not_match_longer_identifier_prefix() {
        // `correctness_v2:` must not match `correctness` as a key —
        // identifier-prefix false-match would conflate distinct axes.
        let h = "correctness_v2: something\n";
        assert_eq!(find_line_anchored_key(h, "correctness"), None);
    }

    #[test]
    fn parse_compare_response_rejects_empty_rationale() {
        // Empty rationale satisfies the schema but defeats halo
        // mitigation — the judge named a winner with zero reasoning
        // committed to text first. Reject as malformed.
        let raw = r#"
correctness:
  rationale: ""
  verdict: a
efficiency:
  rationale: "ok"
  verdict: a
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rationale") && msg.contains("correctness"),
            "expected error to name the offending axis and 'rationale'; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_rejects_whitespace_only_rationale() {
        // Same rationale invariant: a string of spaces/tabs/newlines
        // is no more reasoning than an empty string.
        let raw = "correctness:\n  rationale: \"   \\n  \\t\"\n  verdict: a\nefficiency:\n  rationale: \"ok\"\n  verdict: a\nconciseness:\n  rationale: \"ok\"\n  verdict: tie";
        let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rationale") && msg.contains("correctness"),
            "expected whitespace-only rationale to be rejected; got: {msg}"
        );
    }

    #[test]
    fn parse_compare_response_does_not_false_positive_on_word_inside_rationale_string() {
        // Copilot review: `region.find("verdict")` matched the word
        // "verdict" inside a rationale string body and produced a
        // spurious "verdict before rationale" error. The order check
        // must distinguish key tokens (`verdict:` at line-start) from
        // the same word appearing in narrative text.
        let raw = r#"
correctness:
  rationale: "The agent's verdict was correct and the rationale was sound."
  verdict: a
efficiency:
  rationale: "ok"
  verdict: tie
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let scores = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].verdict, Verdict::BaselineWins);
    }

    #[test]
    fn parse_compare_response_does_not_false_positive_on_word_inside_yaml_comment() {
        // Copilot review variant: a YAML comment between the axis
        // header and the rationale: key containing the word
        // "verdict" would make region.find("verdict") return an
        // offset before region.find("rationale"), triggering a
        // spurious bail. Line-anchored key lookup is the fix.
        let raw = r#"
correctness:
  # The judge weighed the verdict carefully here
  rationale: "ok"
  verdict: a
efficiency:
  rationale: "ok"
  verdict: tie
conciseness:
  rationale: "ok"
  verdict: tie
"#;
        let scores = parse_compare_response(raw, &requested_axes(), false).unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].verdict, Verdict::BaselineWins);
    }

    #[test]
    fn parse_compare_response_rejects_null_yaml_with_clear_error() {
        // Trivially-degenerate YAML scalars (null, {}, whitespace)
        // deserialize to an empty mapping. The naive "missing axis"
        // error from the coverage check is misleading — a maintainer
        // reading "missing axis 'correctness'" would assume partial
        // output, not realize the judge returned null. Surface the
        // empty-mapping case with a clear diagnostic.
        for raw in ["null", "{}", "   \n   "] {
            let err = parse_compare_response(raw, &requested_axes(), false).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.to_lowercase().contains("empty")
                    || msg.to_lowercase().contains("did not parse"),
                "expected empty-mapping diagnostic for {raw:?}; got: {msg}"
            );
        }
    }

    #[test]
    fn parse_compare_response_returns_all_tie_verdicts() {
        // Spec acceptance criterion: "all-tie" verdict shape must be
        // supported. Tie is invariant under swap.
        let raw = r#"
correctness:
  rationale: "Equivalent."
  verdict: tie
efficiency:
  rationale: "Equivalent."
  verdict: tie
conciseness:
  rationale: "Equivalent."
  verdict: tie
"#;
        for swap in [false, true] {
            let scores = parse_compare_response(raw, &requested_axes(), swap).unwrap();
            assert_eq!(scores.len(), 3);
            for score in &scores {
                assert_eq!(
                    score.verdict,
                    Verdict::Tie,
                    "tie must be invariant under swap"
                );
            }
        }
    }

    #[test]
    fn parse_compare_response_returns_baseline_wins_on_every_axis() {
        // Spec acceptance criterion: "baseline-wins on every axis"
        // must be supported. With swap=false, verdict=a maps to
        // BaselineWins. With swap=true, verdict=b maps to BaselineWins.
        let raw_unswapped = r#"
correctness:
  rationale: "Baseline wins."
  verdict: a
efficiency:
  rationale: "Baseline wins."
  verdict: a
conciseness:
  rationale: "Baseline wins."
  verdict: a
"#;
        let scores = parse_compare_response(raw_unswapped, &requested_axes(), false).unwrap();
        for score in &scores {
            assert_eq!(score.verdict, Verdict::BaselineWins);
        }

        let raw_swapped = r#"
correctness:
  rationale: "Baseline wins."
  verdict: b
efficiency:
  rationale: "Baseline wins."
  verdict: b
conciseness:
  rationale: "Baseline wins."
  verdict: b
"#;
        let scores = parse_compare_response(raw_swapped, &requested_axes(), true).unwrap();
        for score in &scores {
            assert_eq!(score.verdict, Verdict::BaselineWins);
        }
    }

    #[tokio::test]
    async fn compare_returns_paired_scores_for_each_requested_axis() {
        let canned = r#"
correctness:
  rationale: "A is more correct."
  verdict: a
efficiency:
  rationale: "Tie on efficiency."
  verdict: tie
conciseness:
  rationale: "B was more concise."
  verdict: b
"#;
        let backend = MockBackend::new(vec![ok_response(canned)]);
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let scores = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &["go".to_string()],
                false,
                None,
            )
            .await
            .unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].verdict, Verdict::BaselineWins); // a + swap=false
        assert_eq!(scores[1].verdict, Verdict::Tie);
        assert_eq!(scores[2].verdict, Verdict::CurrentWins); // b + swap=false
    }

    #[tokio::test]
    async fn compare_with_swap_inverts_a_and_b_to_baseline_and_current() {
        let canned = r#"
correctness:
  rationale: "A is more correct."
  verdict: a
efficiency:
  rationale: "Tie on efficiency."
  verdict: tie
conciseness:
  rationale: "B was more concise."
  verdict: b
"#;
        let backend = MockBackend::new(vec![ok_response(canned)]);
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let scores = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                true,
                None,
            )
            .await
            .unwrap();
        assert_eq!(scores[0].verdict, Verdict::CurrentWins);
        assert_eq!(scores[1].verdict, Verdict::Tie);
        assert_eq!(scores[2].verdict, Verdict::BaselineWins);
    }

    #[tokio::test]
    async fn compare_propagates_transport_error_as_err() {
        let backend = MockBackend::new(vec![Err(anyhow::anyhow!("simulated 503"))]);
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let result = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                false,
                None,
            )
            .await;
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("503") || msg.contains("simulated"));
    }

    #[tokio::test]
    async fn compare_with_swap_yields_inverted_verdicts_on_clear_winner() {
        let canned = r#"
correctness:
  rationale: "A is clearly correct."
  verdict: a
efficiency:
  rationale: "Tie."
  verdict: tie
conciseness:
  rationale: "Tie."
  verdict: tie
"#;
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let axes = [Axis::Correctness, Axis::Efficiency, Axis::Conciseness];

        let backend1 = MockBackend::new(vec![ok_response(canned)]);
        let judge1 = Judge::with_backend(Box::new(backend1), Some("test/model"));
        let scores_unswapped = judge1
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &axes,
                &[],
                false,
                None,
            )
            .await
            .unwrap();

        let backend2 = MockBackend::new(vec![ok_response(canned)]);
        let judge2 = Judge::with_backend(Box::new(backend2), Some("test/model"));
        let scores_swapped = judge2
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &axes,
                &[],
                true,
                None,
            )
            .await
            .unwrap();

        // Clear-winner axis (correctness) inverts under swap.
        assert_eq!(scores_unswapped[0].verdict, Verdict::BaselineWins);
        assert_eq!(scores_swapped[0].verdict, Verdict::CurrentWins);
        // Ties stay ties under swap.
        assert_eq!(scores_unswapped[1].verdict, Verdict::Tie);
        assert_eq!(scores_swapped[1].verdict, Verdict::Tie);
        assert_eq!(scores_unswapped[2].verdict, Verdict::Tie);
        assert_eq!(scores_swapped[2].verdict, Verdict::Tie);
    }

    #[tokio::test]
    async fn compare_returns_err_when_no_judge_model_configured() {
        let backend = MockBackend::new(vec![]);
        let judge = Judge::with_backend(Box::new(backend), None); // no CLI model
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let result = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                false,
                None,
            )
            .await;
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("judge model"), "got: {msg}");
    }

    #[tokio::test]
    async fn compare_bails_on_empty_backend_response_with_model_in_message() {
        // The empty-response branch is distinct from the transport-error
        // branch — `Ok(("".into(), ...))` is a successful call that
        // returned nothing, not a transport failure. Pin both the bail
        // and the model-name diagnostic.
        let backend = MockBackend::new(vec![ok_response("")]);
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let result = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                false,
                None,
            )
            .await;
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("empty") && msg.contains("test/model"),
            "expected both 'empty' and the model name; got: {msg}"
        );
    }

    #[tokio::test]
    async fn compare_with_swap_yields_inverted_mixed_verdicts() {
        // Spec acceptance criterion: same pair fed in swapped order
        // yields inverted-but-otherwise-consistent verdicts. The
        // existing clear-winner test only exercises the `a`-letter
        // inversion path through compare_with_swap; this one round-
        // trips both `a` AND `b` so a future bug that flipped only
        // one direction in the orchestrator would surface.
        let canned = r#"
correctness:
  rationale: "A wins correctness."
  verdict: a
efficiency:
  rationale: "B wins efficiency."
  verdict: b
conciseness:
  rationale: "Tie."
  verdict: tie
"#;
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let axes = [Axis::Correctness, Axis::Efficiency, Axis::Conciseness];

        let backend1 = MockBackend::new(vec![ok_response(canned)]);
        let judge1 = Judge::with_backend(Box::new(backend1), Some("test/model"));
        let unswapped = judge1
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &axes,
                &[],
                false,
                None,
            )
            .await
            .unwrap();

        let backend2 = MockBackend::new(vec![ok_response(canned)]);
        let judge2 = Judge::with_backend(Box::new(backend2), Some("test/model"));
        let swapped = judge2
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &axes,
                &[],
                true,
                None,
            )
            .await
            .unwrap();

        // Both clear-winner axes invert; tie stays tie.
        assert_eq!(unswapped[0].verdict, Verdict::BaselineWins);
        assert_eq!(swapped[0].verdict, Verdict::CurrentWins);
        assert_eq!(unswapped[1].verdict, Verdict::CurrentWins);
        assert_eq!(swapped[1].verdict, Verdict::BaselineWins);
        assert_eq!(unswapped[2].verdict, Verdict::Tie);
        assert_eq!(swapped[2].verdict, Verdict::Tie);
    }

    #[tokio::test]
    async fn compare_transport_error_message_includes_model() {
        // The transport-error context must name the judge model. Once
        // [scoring] overrides land and a scenario can talk to multiple
        // judge models within one eval run, "compare judge call failed:
        // timeout" without a model name leaves the operator guessing
        // which model timed out.
        let backend = MockBackend::new(vec![Err(anyhow::anyhow!("simulated timeout"))]);
        let judge = Judge::with_backend(Box::new(backend), Some("anthropic/claude-haiku-4-5"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let result = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                false,
                None,
            )
            .await;
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("anthropic/claude-haiku-4-5"),
            "transport-error context must include the model name; got: {msg}"
        );
    }

    #[tokio::test]
    async fn compare_with_swap_rejects_empty_axes_slice_as_caller_bug() {
        // Copilot review: `compare(..., axes: &[], ...)` would build a
        // prompt with no axes; an empty YAML mapping (`{}`) from the
        // judge would then return `Ok(vec![])` — a silent no-op
        // masking the caller bug. Bail early instead.
        let backend = MockBackend::new(vec![]); // never called
        let judge = Judge::with_backend(Box::new(backend), Some("test/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let result = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[],
                &[],
                false,
                None,
            )
            .await;
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("axes") && (msg.contains("empty") || msg.contains("non-empty")),
            "expected empty-axes error; got: {msg}"
        );
    }

    /// Canned all-tie response — swap-invariant, so tests of
    /// `Judge::compare` (whose `swap` is randomized) get a deterministic
    /// verdict regardless of which way the RNG falls.
    const ALL_TIE_CANNED: &str = "\
correctness:\n  rationale: \"Equivalent.\"\n  verdict: tie\n\
efficiency:\n  rationale: \"Equivalent.\"\n  verdict: tie\n\
conciseness:\n  rationale: \"Equivalent.\"\n  verdict: tie\n";

    #[tokio::test]
    async fn compare_public_wrapper_threads_args_to_compare_with_swap() {
        // `Judge::compare` is the production wrapper called by the
        // report orchestrator; `compare_with_swap` is the test seam.
        // Without a dedicated test on `compare`, a future refactor
        // that reorders args between the two (both take `ComparePair`,
        // `&[Axis]`, `&[String]`, and `Option<&str>` — all distinct
        // types but reference-typed slices type-check across
        // orderings) would pass every existing test while silently
        // breaking production.
        //
        // Uses ALL_TIE_CANNED so the random `swap` doesn't matter:
        // tie verdicts are swap-invariant.
        let recorder = Arc::new(Mutex::new(Vec::new()));
        let backend = MockBackend::with_model_recorder(
            vec![ok_response(ALL_TIE_CANNED)],
            Arc::clone(&recorder),
        );
        let judge = Judge::with_backend(Box::new(backend), Some("cli/model"));
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let scores = judge
            .compare(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &["go".to_string()],
                None,
            )
            .await
            .unwrap();
        assert_eq!(scores.len(), 3);
        for score in &scores {
            assert_eq!(score.verdict, Verdict::Tie);
        }
        // Verify the orchestrator passed `cli/model` (CLI precedence)
        // through to the backend — pins the model-threading contract.
        let recorded = recorder.lock().unwrap();
        assert_eq!(
            recorded.as_slice(),
            &["cli/model".to_string()],
            "expected the CLI model to be threaded through to the backend",
        );
    }

    #[tokio::test]
    async fn compare_with_swap_falls_back_to_scenario_judge_model_when_cli_is_none() {
        // The pure `resolve_judge_model` is tested at the function
        // level, but the integration through `compare_with_swap` was
        // not — every prior compare test passed `cli_model=Some(...)`
        // via `Judge::with_backend(_, Some(...))`. A bug where
        // `compare_with_swap` swapped the second and third args to
        // `resolve_judge_model` (CLI vs scenario) would not be caught
        // by any existing test.
        let recorder = Arc::new(Mutex::new(Vec::new()));
        let backend = MockBackend::with_model_recorder(
            vec![ok_response(ALL_TIE_CANNED)],
            Arc::clone(&recorder),
        );
        let judge = Judge::with_backend(Box::new(backend), None /* no CLI override */);
        let baseline = make_test_transcript(vec![]);
        let current = make_test_transcript(vec![]);
        let scores = judge
            .compare_with_swap(
                ComparePair {
                    baseline: &baseline,
                    current: &current,
                },
                &[Axis::Correctness, Axis::Efficiency, Axis::Conciseness],
                &[],
                false,
                Some("scenario/model"),
            )
            .await
            .unwrap();
        assert_eq!(scores.len(), 3);
        let recorded = recorder.lock().unwrap();
        assert_eq!(
            recorded.as_slice(),
            &["scenario/model".to_string()],
            "expected the scenario judge model to be used when CLI is None",
        );
    }
}
