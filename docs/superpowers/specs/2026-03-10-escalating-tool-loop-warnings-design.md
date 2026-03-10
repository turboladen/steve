# Escalating Tool Loop Warnings

**Date**: 2026-03-10
**Status**: Approved

## Problem

The LLM can enter unbounded exploration loops — re-reading files, making 1 tool call per iteration with ~100 tokens of output — burning through the 75-iteration safety limit with no prior signal. The hard kill at 75 is jarring: the user gets an error with no explanation, and hundreds of thousands of prompt tokens are wasted.

Real-world example: a doc cross-referencing task produced 76 iterations, 380K prompt tokens, 165 messages, and 9 cache hits on the same file before termination.

## Solution

Append escalating warning text to tool result messages at iteration thresholds. Show corresponding notifications in the TUI.

## Thresholds

Computed as percentages of `MAX_TOOL_ITERATIONS` (currently 75):

| Level | % | At 75 | Appended to tool result | TUI |
|-------|---|-------|------------------------|-----|
| Nudge | 33% | 25 | `[Note: You have made {n} tool calls. Begin wrapping up your analysis.]` | System info |
| Warning | 67% | 50 | `[Warning: {n}/{max} tool calls used. Finish within the next few calls. Do not re-read files.]` | System warning |
| Critical | 87% | 65 | `[CRITICAL: {remaining} calls remaining before forced termination. Respond NOW.]` | System warning |
| Kill | 100% | 75 | (existing hard kill) | LlmError (existing) |

## Implementation

### Injection point

In `stream.rs`, after tool results are appended to `messages` (end of phase 2 + phase 3), before the loop continues. Mutate the last `Tool` message's content string to append the warning text.

### Dedup

Use a `u8` bitmask (`warnings_sent`) to track which thresholds have fired. Reset when `iteration_count` resets (user permission grant or interjection).

### TUI notification

Send `AppEvent::StreamNotice` with warning text so the user sees why the LLM is wrapping up. Handled in `app.rs` as `MessageBlock::System`.

## Files changed

- `src/stream.rs` — warning injection logic, bitmask tracking
- `src/event.rs` — `AppEvent::StreamNotice` variant
- `src/app.rs` — `StreamNotice` handler

## Not changed

- `MAX_TOOL_ITERATIONS` stays at 75 (not configurable)
- No changes to compressor, cache, or permission logic
- Hard kill behavior unchanged
