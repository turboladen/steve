//! Length/iteration recovery logic for finish_reason=Length and iteration limits.

/// Context pressure threshold: if prompt_tokens > this % of context_window,
/// we consider the context full (as opposed to output token limit truncation).
pub(super) const CONTEXT_PRESSURE_PCT: u64 = 85;

/// Whether `finish_reason=Length` was caused by context pressure or output truncation.
pub(super) enum LengthCause {
    /// Context window nearly full (prompt_tokens > CONTEXT_PRESSURE_PCT% of window).
    ContextPressure,
    /// Model hit output token limit, context is fine.
    OutputTruncation,
}

/// Messages for `finish_reason=Length` recovery, parameterized by cause and scenario.
pub(super) struct LengthRecoveryMessages {
    pub system_msg: &'static str,
    pub notice: &'static str,
    pub error_msg: &'static str,
}

/// Determine whether `finish_reason=Length` is due to context pressure or output truncation.
pub(super) fn classify_length_cause(
    prompt_tokens: u32,
    context_window: Option<u64>,
) -> LengthCause {
    let pressured = context_window
        .map(|cw| cw > 0 && (prompt_tokens as u64) > cw * CONTEXT_PRESSURE_PCT / 100)
        .unwrap_or(true); // Assume pressured if we don't know the window
    if pressured {
        LengthCause::ContextPressure
    } else {
        LengthCause::OutputTruncation
    }
}

/// Get recovery messages for the "no tool calls" path (response was cut off).
pub(super) fn length_recovery_no_tools(cause: &LengthCause) -> LengthRecoveryMessages {
    match cause {
        LengthCause::ContextPressure => LengthRecoveryMessages {
            system_msg: "[SYSTEM: Context window nearly full. Your previous response was \
                         cut off. Provide a concise but complete response NOW. Do not \
                         call any tools.]",
            notice: "⚙ Context nearly full — compressing and retrying",
            error_msg: "Context window nearly full — response was cut off. \
                        Run /compact to free space, or /new to start fresh.",
        },
        LengthCause::OutputTruncation => LengthRecoveryMessages {
            system_msg: "[SYSTEM: Your response was cut off (output token limit reached). \
                         Provide a concise but complete response NOW. Do not call any tools.]",
            notice: "⚙ Output truncated — retrying without tools",
            error_msg: "Output was truncated (model may have hit an output token limit). \
                        Try a shorter query or /compact.",
        },
    }
}

/// Get recovery messages for the "all tool calls truncated" path.
pub(super) fn length_recovery_truncated_tools(cause: &LengthCause) -> LengthRecoveryMessages {
    match cause {
        LengthCause::ContextPressure => LengthRecoveryMessages {
            system_msg: "[SYSTEM: Context window nearly full — your tool calls were \
                         truncated. Provide a concise but complete response NOW using \
                         the information you already have. Do not call any tools.]",
            notice: "⚙ Context nearly full — compressing tool calls and retrying",
            error_msg: "Context window nearly full — tool calls were truncated. \
                        Run /compact to free space, or /new to start fresh.",
        },
        LengthCause::OutputTruncation => LengthRecoveryMessages {
            system_msg: "[SYSTEM: Your tool calls were truncated (output token limit reached). \
                         Provide a concise but complete response NOW using the information you \
                         already have. Do not call any tools.]",
            notice: "⚙ Output truncated — retrying without tools",
            error_msg: "Output was truncated — tool calls were cut off \
                        (model may have hit an output token limit). \
                        Try a shorter query or /compact.",
        },
    }
}

/// Warning thresholds as percentages of the max iteration limit.
pub(super) const WARN_NUDGE_PCT: u32 = 20;
pub(super) const WARN_WARNING_PCT: u32 = 47;
pub(super) const WARN_CRITICAL_PCT: u32 = 73;
pub(super) const WARN_FINAL_PCT: u32 = 93;

/// Bitmask flags for tracking which warnings have been sent.
pub(super) const WARN_NUDGE_BIT: u8 = 0b0001;
pub(super) const WARN_WARNING_BIT: u8 = 0b0010;
pub(super) const WARN_CRITICAL_BIT: u8 = 0b0100;
pub(super) const WARN_FINAL_BIT: u8 = 0b1000;

/// Check whether an escalating warning should be emitted at the current iteration count.
/// Returns the warning text to append to the last tool result, if any, and updates the
/// bitmask to prevent re-firing the same threshold.
pub(super) fn check_iteration_warning(
    iteration_count: u32,
    max_iterations: u32,
    warnings_sent: &mut u8,
) -> Option<String> {
    let final_at = max_iterations * WARN_FINAL_PCT / 100;
    let critical_at = max_iterations * WARN_CRITICAL_PCT / 100;
    let warning_at = max_iterations * WARN_WARNING_PCT / 100;
    let nudge_at = max_iterations * WARN_NUDGE_PCT / 100;

    if iteration_count >= final_at && (*warnings_sent & WARN_FINAL_BIT) == 0 {
        *warnings_sent |= WARN_FINAL_BIT;
        let remaining = max_iterations.saturating_sub(iteration_count);
        Some(format!(
            "\n\n[FINAL: {remaining} iterations before forced termination. Tools are already \
             revoked. RESPOND NOW with your complete answer.]"
        ))
    } else if iteration_count >= critical_at && (*warnings_sent & WARN_CRITICAL_BIT) == 0 {
        *warnings_sent |= WARN_CRITICAL_BIT;
        Some(
            "\n\n[CRITICAL: Tools REMOVED from next request. You cannot make more tool calls. \
             Respond with your findings immediately.]"
                .to_string(),
        )
    } else if iteration_count >= warning_at && (*warnings_sent & WARN_WARNING_BIT) == 0 {
        *warnings_sent |= WARN_WARNING_BIT;
        Some(format!(
            "\n\n[WARNING: {iteration_count}/{max_iterations} tool calls used. Tool access \
             will be REVOKED at ~{critical_at} calls. Wrap up NOW.]"
        ))
    } else if iteration_count >= nudge_at && (*warnings_sent & WARN_NUDGE_BIT) == 0 {
        *warnings_sent |= WARN_NUDGE_BIT;
        Some(format!(
            "\n\n[You have made {iteration_count} tool calls. Begin synthesizing. \
             The system will revoke tool access at ~{critical_at} calls.]"
        ))
    } else {
        None
    }
}

/// Classify whether an OpenAI API error is transient (worth retrying).
///
/// Transient errors include network timeouts, connection failures, rate limits,
/// and server overload responses. Non-transient errors (auth failures, invalid
/// arguments, deserialization errors) are not retried.
pub(super) fn is_transient_error(err: &async_openai::error::OpenAIError) -> bool {
    use async_openai::error::OpenAIError;
    match err {
        OpenAIError::Reqwest(e) => {
            // Network-level failures: timeout, connection refused, DNS, 5xx, etc.
            e.is_timeout()
                || e.is_connect()
                || e.is_request()
                || e.status().is_some_and(|s| s.is_server_error())
        }
        OpenAIError::ApiError(api_err) => {
            // Rate limit or server overload from the API
            matches!(api_err.code.as_deref(), Some("rate_limit_exceeded"))
                || api_err.message.contains("overloaded")
                || api_err.message.contains("temporarily unavailable")
        }
        OpenAIError::StreamError(_) => {
            // SSE stream failures are often transient (connection drops)
            true
        }
        // Explicit non-transient variants — exhaustive match ensures new variants
        // are reviewed for transient-ness when async-openai adds them.
        OpenAIError::JSONDeserialize(_, _) => false,
        OpenAIError::FileSaveError(_) => false,
        OpenAIError::FileReadError(_) => false,
        OpenAIError::InvalidArgument(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::error::{ApiError, OpenAIError, StreamError};

    /// Helper to build an ApiError with a given message and optional code.
    fn make_api_error(message: &str, code: Option<&str>) -> OpenAIError {
        OpenAIError::ApiError(ApiError {
            message: message.to_string(),
            r#type: None,
            param: None,
            code: code.map(String::from),
        })
    }

    #[test]
    fn transient_rate_limit() {
        let err = make_api_error("Rate limit exceeded", Some("rate_limit_exceeded"));
        assert!(
            is_transient_error(&err),
            "rate_limit_exceeded should be transient"
        );
    }

    #[test]
    fn transient_overloaded() {
        let err = make_api_error("The server is overloaded, please try again later", None);
        assert!(
            is_transient_error(&err),
            "overloaded message should be transient"
        );
    }

    #[test]
    fn transient_temporarily_unavailable() {
        let err = make_api_error("Service is temporarily unavailable", None);
        assert!(
            is_transient_error(&err),
            "temporarily unavailable message should be transient"
        );
    }

    #[test]
    fn not_transient_auth_error() {
        let err = make_api_error("Invalid API key provided", Some("invalid_api_key"));
        assert!(
            !is_transient_error(&err),
            "invalid_api_key should not be transient"
        );
    }

    #[test]
    fn not_transient_invalid_argument() {
        let err = OpenAIError::InvalidArgument("bad argument".to_string());
        assert!(
            !is_transient_error(&err),
            "InvalidArgument should not be transient"
        );
    }

    #[test]
    fn not_transient_json_deserialize() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let err = OpenAIError::JSONDeserialize(json_err, "invalid json".to_string());
        assert!(
            !is_transient_error(&err),
            "JSONDeserialize should not be transient"
        );
    }

    #[test]
    fn not_transient_generic_api_error() {
        let err = make_api_error("Something went wrong", Some("server_error"));
        assert!(
            !is_transient_error(&err),
            "generic server_error without overloaded/unavailable message should not be transient"
        );
    }

    #[test]
    fn transient_stream_error() {
        let stream_err = StreamError::EventStream("connection reset".to_string());
        let err = OpenAIError::StreamError(Box::new(stream_err));
        assert!(is_transient_error(&err), "StreamError should be transient");
    }

    // -- check_iteration_warning tests --

    #[test]
    fn warning_nudge_fires_at_20_percent() {
        let mut sent = 0u8;
        // At 75 max, nudge_at = 75 * 20 / 100 = 15
        assert!(check_iteration_warning(14, 75, &mut sent).is_none());
        let text = check_iteration_warning(15, 75, &mut sent);
        assert!(text.is_some());
        assert!(text.unwrap().contains("15 tool calls"));
        assert_eq!(sent, WARN_NUDGE_BIT);
    }

    #[test]
    fn warning_does_not_repeat() {
        let mut sent = 0u8;
        let first = check_iteration_warning(15, 75, &mut sent);
        assert!(first.is_some());
        let second = check_iteration_warning(16, 75, &mut sent);
        assert!(second.is_none(), "nudge should not fire twice");
    }

    #[test]
    fn warning_escalates_through_all_levels() {
        let mut sent = 0u8;
        // Nudge at 20% (75 * 20 / 100 = 15)
        let nudge = check_iteration_warning(15, 75, &mut sent);
        assert!(nudge.is_some());
        assert!(nudge.unwrap().contains("Begin synthesizing"));
        // Warning at 47% (75 * 47 / 100 = 35)
        let warn = check_iteration_warning(35, 75, &mut sent);
        assert!(warn.is_some());
        assert!(warn.unwrap().contains("WARNING:"));
        // Critical at 73% (75 * 73 / 100 = 54)
        let crit = check_iteration_warning(54, 75, &mut sent);
        assert!(crit.is_some());
        assert!(crit.unwrap().contains("CRITICAL:"));
        // Final at 93% (75 * 93 / 100 = 69)
        let fin = check_iteration_warning(69, 75, &mut sent);
        assert!(fin.is_some());
        assert!(fin.unwrap().contains("FINAL:"));
        // All bits set
        assert_eq!(
            sent,
            WARN_NUDGE_BIT | WARN_WARNING_BIT | WARN_CRITICAL_BIT | WARN_FINAL_BIT
        );
    }

    #[test]
    fn warning_critical_shows_remaining_count() {
        let mut sent = 0u8;
        // Jump straight to critical at 54 (75 * 73 / 100 = 54)
        let text = check_iteration_warning(54, 75, &mut sent).unwrap();
        assert!(text.contains("Tools REMOVED"));
    }

    #[test]
    fn warning_reset_allows_refiring() {
        let mut sent = 0u8;
        check_iteration_warning(15, 75, &mut sent);
        assert_ne!(sent, 0);
        // Simulate reset (user granted permission)
        sent = 0;
        let text = check_iteration_warning(15, 75, &mut sent);
        assert!(text.is_some(), "should fire again after reset");
    }

    #[test]
    fn warning_below_all_thresholds_returns_none() {
        let mut sent = 0u8;
        assert!(check_iteration_warning(1, 75, &mut sent).is_none());
        assert!(check_iteration_warning(10, 75, &mut sent).is_none());
        assert!(check_iteration_warning(14, 75, &mut sent).is_none());
        assert_eq!(sent, 0);
    }

    #[test]
    fn warning_highest_level_wins_on_jump() {
        let mut sent = 0u8;
        // Jump from 0 to 70 — should fire final (highest), not nudge
        let text = check_iteration_warning(70, 75, &mut sent).unwrap();
        assert!(text.contains("FINAL:"));
        assert_eq!(sent & WARN_FINAL_BIT, WARN_FINAL_BIT);
    }

    #[test]
    fn warning_plan_mode_lower_max() {
        let mut sent = 0u8;
        // Plan mode max = 40, nudge at 20% = 8
        assert!(check_iteration_warning(7, 40, &mut sent).is_none());
        let text = check_iteration_warning(8, 40, &mut sent);
        assert!(text.is_some());
        assert!(text.unwrap().contains("8 tool calls"));
        // Warning at 47% (40 * 47 / 100 = 18)
        let warn = check_iteration_warning(18, 40, &mut sent);
        assert!(warn.is_some());
        assert!(warn.unwrap().contains("WARNING:"));
        // Critical at 73% (40 * 73 / 100 = 29)
        let crit = check_iteration_warning(29, 40, &mut sent);
        assert!(crit.is_some());
        assert!(crit.unwrap().contains("CRITICAL:"));
        // Final at 93% (40 * 93 / 100 = 37)
        let fin = check_iteration_warning(37, 40, &mut sent);
        assert!(fin.is_some());
        assert!(fin.unwrap().contains("FINAL:"));
    }

    #[test]
    fn warning_final_tier_shows_remaining() {
        let mut sent = 0u8;
        // At 75 max, final fires at 69 (75 * 93 / 100)
        let text = check_iteration_warning(69, 75, &mut sent).unwrap();
        assert!(text.contains("6 iterations"));
        assert!(text.contains("FINAL:"));
    }

    #[test]
    fn warning_nudge_includes_critical_threshold() {
        let mut sent = 0u8;
        let text = check_iteration_warning(15, 75, &mut sent).unwrap();
        // Nudge should tell the LLM when tools will be revoked
        assert!(text.contains("revoke tool access"));
        // Should include the critical threshold (~54 for max 75)
        assert!(text.contains("54"));
    }

    #[test]
    fn warning_at_level_includes_critical_threshold() {
        let mut sent = 0u8;
        let text = check_iteration_warning(35, 75, &mut sent).unwrap();
        // Warning should mention when tools will be REVOKED
        assert!(text.contains("REVOKED"));
        assert!(text.contains("54"));
    }

    #[test]
    fn warning_critical_mentions_tools_removed() {
        let mut sent = 0u8;
        let text = check_iteration_warning(54, 75, &mut sent).unwrap();
        assert!(text.contains("CRITICAL:"));
        assert!(text.contains("Tools REMOVED"));
    }

    #[test]
    fn plan_mode_critical_at_40_iterations() {
        // Plan mode max is 55. At 73%, tools strip at iteration 40
        // (same as old hard limit), giving graceful degradation.
        let plan_max = 55u32;
        let critical_at = plan_max * WARN_CRITICAL_PCT / 100;
        assert_eq!(critical_at, 40);
        // Verify warnings fire at expected plan-mode thresholds
        let mut sent = 0u8;
        let nudge = check_iteration_warning(11, plan_max, &mut sent);
        assert!(nudge.is_some()); // 55 * 20 / 100 = 11
    }
}
