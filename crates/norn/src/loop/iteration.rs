//! Threshold detection, soft handoff, continuation.
//!
//! Runtime monitors that observe agent loop state after each iteration and
//! emit advisory [`IterationSignal`]s when the loop is approaching a token
//! budget, looping on the same error, or producing low-quality output.
//!
//! Monitors are observation-only: they do not terminate the loop (that
//! responsibility stays with the schema budget and max-iterations from
//! N-005). The agent loop is expected to call [`evaluate_iteration`] after
//! every iteration and either inject guidance, emit warnings, or update
//! profile state in response to the returned signals.

use std::collections::HashMap;
use std::fmt::Write as _;

use regex::Regex;

use crate::r#loop::numeric::token_count_to_f64;
use crate::provider::usage::Usage;

/// Hardcoded terminal phrases used by premature-completion detection.
///
/// A short assistant message containing one of these phrases is treated as
/// the model wrapping up prematurely (e.g., closing pleasantries with no
/// substantive content). Phrases are matched case-insensitively.
const TERMINAL_PHRASES: &[&str] = &[
    "I hope this helps",
    "Let me know if",
    "Is there anything else",
];

/// Maximum number of characters on each side of a hedging match when
/// building the surrounding text excerpt.
const EXCERPT_HALF_WINDOW: usize = 40;

/// Character-count threshold below which terminal phrases are treated as
/// premature-completion signals.
const PREMATURE_COMPLETION_CHAR_LIMIT: usize = 50;

/// Configuration for the iteration monitors.
///
/// All thresholds are optional in effect: a `context_window_tokens` of zero
/// disables token-budget monitoring, an empty `hedging_patterns` list
/// disables hedging detection, and a `failure_repeat_window` of zero
/// disables repeated-failure detection.
#[derive(Clone, Debug)]
pub struct IterationMonitorConfig {
    /// Total token budget available for the agent step.
    pub context_window_tokens: u64,
    /// Fraction of the budget (0.0-1.0) at which to emit a soft warning.
    pub warn_threshold_pct: f64,
    /// Fraction of the budget (0.0-1.0) at which to inject handoff guidance.
    pub handoff_threshold_pct: f64,
    /// Wrap-up guidance text injected when the handoff threshold is crossed.
    pub handoff_guidance: String,
    /// Number of **consecutive failing iterations** sharing one
    /// normalized error signature required to trigger
    /// [`IterationSignal::RepeatedFailure`].
    ///
    /// The window counts iterations (provider round-trips), not raw error
    /// strings: a single iteration whose tool batch produces N identical
    /// errors is one failed attempt, because the model has had no chance
    /// to react yet. The signal therefore means "the model has seen this
    /// error and re-produced it `failure_repeat_window` iterations in a
    /// row". After firing, the signal re-arms only once another full
    /// window of consecutive failing iterations accumulates, and an
    /// error-free iteration resets every streak.
    pub failure_repeat_window: usize,
    /// Regex patterns used for hedging-language detection. Patterns that
    /// fail to compile are logged at `warn` level and skipped.
    pub hedging_patterns: Vec<String>,
}

impl Default for IterationMonitorConfig {
    /// Default configuration is fully inert: no monitors fire.
    ///
    /// `context_window_tokens` is zero so the token-budget check short-
    /// circuits to [`IterationSignal::None`]. Thresholds default to `1.0`
    /// so that even if a caller sets a non-zero window without overriding
    /// thresholds, signals only fire at full utilization rather than
    /// triggering immediately. `failure_repeat_window` is zero so the
    /// repeated-failure check is disabled, and the hedging pattern list
    /// is empty. Callers should configure each field explicitly for the
    /// behavior they want.
    fn default() -> Self {
        Self {
            context_window_tokens: 0,
            warn_threshold_pct: 1.0,
            handoff_threshold_pct: 1.0,
            handoff_guidance: String::new(),
            failure_repeat_window: 0,
            hedging_patterns: Vec::new(),
        }
    }
}

/// A specific low-quality signal detected in assistant text.
#[derive(Clone, Debug)]
pub enum QualitySignal {
    /// A configured hedging regex matched the assistant text.
    Hedging {
        /// The regex pattern that matched (as supplied in config).
        matched_pattern: String,
        /// Text excerpt centred on the match, up to
        /// `EXCERPT_HALF_WINDOW` characters on each side.
        text_excerpt: String,
    },
    /// A short assistant message contained a terminal phrase, suggesting
    /// the model is wrapping up before producing substantive output.
    PrematureCompletion {
        /// The full assistant text (it is short by construction).
        text_excerpt: String,
    },
}

/// Result of evaluating a single iteration monitor.
///
/// `None` is the inert variant returned when no signal applies. Callers
/// typically discard `None` values before forwarding signals to the
/// orchestrator.
#[derive(Clone, Debug)]
pub enum IterationSignal {
    /// No signal — the monitor did not trigger.
    None,
    /// Token usage has crossed the warning threshold but not the handoff
    /// threshold.
    TokenWarning {
        /// Cumulative tokens consumed (input + output, excluding cache).
        used: u64,
        /// Configured `context_window_tokens` budget.
        limit: u64,
        /// Fraction of the budget consumed (0.0-1.0).
        pct: f64,
    },
    /// Token usage has crossed the handoff threshold — the agent should
    /// wrap up and summarize progress.
    HandoffTriggered {
        /// Cumulative tokens consumed (input + output, excluding cache).
        used: u64,
        /// Configured `context_window_tokens` budget.
        limit: u64,
        /// Wrap-up guidance text from config, cloned here for delivery.
        guidance: String,
    },
    /// The same (normalized) error has appeared in
    /// `failure_repeat_window` consecutive failing iterations.
    RepeatedFailure {
        /// Normalized error signature shared by the consecutive
        /// failing iterations.
        error_signature: String,
        /// Length of the consecutive failing-iteration streak for this
        /// signature at the moment the signal fired.
        consecutive_count: usize,
    },
    /// One or more quality signals were detected in the assistant text.
    QualityWarning {
        /// Specific quality signals (hedging matches and/or premature
        /// completion).
        signals: Vec<QualitySignal>,
    },
}

/// Mutable per-step state for the iteration monitors.
///
/// Maintained by the caller across iterations of a single agent step.
/// The state suppresses duplicate token-budget signals (each fires at most
/// once per step) and tracks, per normalized error signature, how many
/// consecutive failing iterations have produced it.
#[derive(Clone, Debug, Default)]
pub struct IterationMonitorState {
    /// Whether [`IterationSignal::HandoffTriggered`] has already fired
    /// this step.
    pub handoff_fired: bool,
    /// Whether [`IterationSignal::TokenWarning`] has already fired this
    /// step.
    pub warn_fired: bool,
    /// Consecutive failing-iteration streaks keyed by normalized error
    /// signature. A signature missing from one iteration's error batch
    /// loses its streak; an error-free iteration clears the map.
    failure_streaks: HashMap<String, FailureStreak>,
}

/// One signature's consecutive failing-iteration streak.
#[derive(Clone, Copy, Debug)]
struct FailureStreak {
    /// Consecutive failing iterations that produced this signature.
    consecutive_iterations: usize,
    /// Streak length at the last [`IterationSignal::RepeatedFailure`]
    /// fire for this signature; `0` when it has never fired. A re-fire
    /// requires another full window of new failing iterations beyond
    /// this watermark, so sustained failures escalate at window
    /// boundaries instead of spamming a signal every iteration.
    fired_at: usize,
}

/// Check cumulative token usage against the configured thresholds.
///
/// Returns [`IterationSignal::HandoffTriggered`] when usage is at or above
/// `handoff_threshold_pct`, [`IterationSignal::TokenWarning`] when usage
/// is at or above `warn_threshold_pct` but below the handoff threshold,
/// and [`IterationSignal::None`] otherwise. A `context_window_tokens` of
/// zero is treated as monitoring-disabled.
///
/// Only `input_tokens + output_tokens` are counted; cache read/write
/// tokens are deliberately excluded so that aggressive cache use does not
/// artificially inflate the budget consumption figure.
#[must_use]
pub fn check_token_threshold(usage: &Usage, config: &IterationMonitorConfig) -> IterationSignal {
    if config.context_window_tokens == 0 {
        return IterationSignal::None;
    }
    let used: u64 = usage.input_tokens.saturating_add(usage.output_tokens);
    let pct = token_count_to_f64(used) / token_count_to_f64(config.context_window_tokens);
    if pct >= config.handoff_threshold_pct {
        IterationSignal::HandoffTriggered {
            used,
            limit: config.context_window_tokens,
            guidance: config.handoff_guidance.clone(),
        }
    } else if pct >= config.warn_threshold_pct {
        IterationSignal::TokenWarning {
            used,
            limit: config.context_window_tokens,
            pct,
        }
    } else {
        IterationSignal::None
    }
}

/// Format a wrap-up message for injection into the conversation.
///
/// When `signal` is [`IterationSignal::HandoffTriggered`], returns a
/// multi-line string containing the configured guidance text, the current
/// usage percentage, and an explicit instruction to summarize progress and
/// prepare for continuation. For any other variant the function returns an
/// empty string — callers should only invoke it on `HandoffTriggered`.
#[must_use]
pub fn format_handoff_message(signal: &IterationSignal) -> String {
    let IterationSignal::HandoffTriggered {
        used,
        limit,
        guidance,
    } = signal
    else {
        return String::new();
    };
    let pct_display = if *limit == 0 {
        0.0
    } else {
        (token_count_to_f64(*used) / token_count_to_f64(*limit)) * 100.0
    };
    let mut message = String::new();
    message.push_str(guidance);
    if !guidance.is_empty() {
        message.push_str("\n\n");
    }
    let _ = write!(
        message,
        "Current token usage: {used} / {limit} ({pct_display:.1}%).\n\n",
    );
    message.push_str(
        "Please summarize progress so far and prepare for continuation in a fresh session. \
         Capture outstanding work, key findings, and next steps so a successor can resume \
         without re-deriving context.",
    );
    message
}

/// Compute the normalized error signature used by repeated-failure detection.
///
/// Normalization steps: trim leading/trailing whitespace, lowercase the
/// remaining text, and strip ASCII digits. This collapses errors that
/// differ only in line numbers, byte offsets, or other numeric literals
/// into a single signature so that "Error at line 5: foo" and "Error at
/// line 12: foo" are treated as the same recurring error.
fn normalize_error(error: &str) -> String {
    error
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .collect()
}

/// Record one iteration's error batch and detect repeated failures.
///
/// `errors` is everything the just-completed iteration produced (tool
/// errors and schema-validation failures). Each distinct normalized
/// signature in the batch extends its consecutive-iteration streak by
/// one; signatures absent from the batch lose their streaks, and an
/// empty batch (a clean iteration) clears every streak.
///
/// A [`IterationSignal::RepeatedFailure`] fires for a signature when its
/// streak reaches `window` consecutive iterations, and re-fires only
/// after a further full `window` of consecutive failing iterations
/// accumulates beyond the previous fire (see
/// [`IterationMonitorConfig::failure_repeat_window`] for the rationale).
/// Signals are emitted in lexicographic signature order so output is
/// deterministic when several signatures trip in the same iteration.
///
/// A `window` of zero disables detection entirely.
pub fn observe_failure_iteration(
    state: &mut IterationMonitorState,
    errors: &[String],
    window: usize,
) -> Vec<IterationSignal> {
    if window == 0 {
        return Vec::new();
    }
    if errors.is_empty() {
        // Recovery: a clean iteration breaks every streak.
        state.failure_streaks.clear();
        return Vec::new();
    }

    let mut signatures: Vec<String> = errors.iter().map(|e| normalize_error(e)).collect();
    signatures.sort();
    signatures.dedup();

    let mut next_streaks: HashMap<String, FailureStreak> = HashMap::with_capacity(signatures.len());
    let mut signals = Vec::new();
    for signature in signatures {
        let mut streak = state
            .failure_streaks
            .get(&signature)
            .copied()
            .unwrap_or(FailureStreak {
                consecutive_iterations: 0,
                fired_at: 0,
            });
        streak.consecutive_iterations = streak.consecutive_iterations.saturating_add(1);
        if streak.consecutive_iterations >= window
            && streak
                .consecutive_iterations
                .saturating_sub(streak.fired_at)
                >= window
        {
            streak.fired_at = streak.consecutive_iterations;
            signals.push(IterationSignal::RepeatedFailure {
                error_signature: signature.clone(),
                consecutive_count: streak.consecutive_iterations,
            });
        }
        next_streaks.insert(signature, streak);
    }
    // Signatures not seen this iteration are dropped: streaks are over
    // *consecutive* iterations.
    state.failure_streaks = next_streaks;
    signals
}

/// Scan assistant text for hedging language and premature-completion patterns.
///
/// For each pattern in `hedging_patterns` the function compiles the
/// pattern as a regex and records the first match (if any) as a
/// [`QualitySignal::Hedging`]. Patterns that fail to compile are logged at
/// `warn` level and skipped — they never cause this function to fail.
///
/// If the text is shorter than `PREMATURE_COMPLETION_CHAR_LIMIT`
/// characters and contains any of `TERMINAL_PHRASES` (case-insensitive),
/// a single [`QualitySignal::PrematureCompletion`] is recorded.
///
/// Returns [`IterationSignal::QualityWarning`] when any signals were
/// found, otherwise [`IterationSignal::None`].
#[must_use]
pub fn check_quality_signals(text: &str, hedging_patterns: &[String]) -> IterationSignal {
    let mut signals: Vec<QualitySignal> = Vec::new();

    for pattern in hedging_patterns {
        match Regex::new(pattern) {
            Ok(re) => {
                if let Some(m) = re.find(text) {
                    signals.push(QualitySignal::Hedging {
                        matched_pattern: pattern.clone(),
                        text_excerpt: build_excerpt(text, m.start(), m.end()),
                    });
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    pattern = pattern.as_str(),
                    "failed to compile hedging pattern; skipping",
                );
            }
        }
    }

    if text.chars().count() < PREMATURE_COMPLETION_CHAR_LIMIT {
        let lowered = text.to_lowercase();
        for phrase in TERMINAL_PHRASES {
            if lowered.contains(&phrase.to_lowercase()) {
                signals.push(QualitySignal::PrematureCompletion {
                    text_excerpt: text.to_string(),
                });
                break;
            }
        }
    }

    if signals.is_empty() {
        IterationSignal::None
    } else {
        IterationSignal::QualityWarning { signals }
    }
}

/// Build a char-boundary-safe excerpt of `text` centred on the byte range
/// `[match_start, match_end)`.
///
/// Pads up to `EXCERPT_HALF_WINDOW` characters of context on each side
/// while staying within `text` and snapping any boundary that falls in the
/// middle of a UTF-8 sequence outward to a valid char boundary. This
/// guarantees the resulting slice is a valid `&str` even when the match
/// is adjacent to multibyte characters.
fn build_excerpt(text: &str, match_start: usize, match_end: usize) -> String {
    let desired_start = match_start.saturating_sub(EXCERPT_HALF_WINDOW);
    let desired_end = match_end
        .saturating_add(EXCERPT_HALF_WINDOW)
        .min(text.len());

    let mut start = desired_start;
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = desired_end;
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }

    text[start..end].to_string()
}

/// Run all iteration monitors and return any non-`None` signals.
///
/// This is the single entry point the agent loop calls after each
/// iteration. It runs in order: token-threshold check, repeated-failure
/// check, and quality-signal check. Token-budget signals are suppressed
/// after their first firing per step (tracked via [`IterationMonitorState::
/// handoff_fired`] and [`IterationMonitorState::warn_fired`]).
///
/// `latest_errors` carries the just-completed iteration's failures:
/// `Some(&[..])` records a failing iteration, `Some(&[])` records a clean
/// iteration (resetting every repeated-failure streak), and `None` means
/// the caller has no failure information for this iteration, leaving the
/// streaks untouched.
pub fn evaluate_iteration(
    state: &mut IterationMonitorState,
    usage: &Usage,
    latest_text: Option<&str>,
    latest_errors: Option<&[String]>,
    config: &IterationMonitorConfig,
) -> Vec<IterationSignal> {
    let mut out: Vec<IterationSignal> = Vec::new();

    match check_token_threshold(usage, config) {
        IterationSignal::HandoffTriggered {
            used,
            limit,
            guidance,
        } => {
            if !state.handoff_fired {
                state.handoff_fired = true;
                out.push(IterationSignal::HandoffTriggered {
                    used,
                    limit,
                    guidance,
                });
            }
        }
        IterationSignal::TokenWarning { used, limit, pct } => {
            if !state.warn_fired {
                state.warn_fired = true;
                out.push(IterationSignal::TokenWarning { used, limit, pct });
            }
        }
        IterationSignal::None
        | IterationSignal::RepeatedFailure { .. }
        | IterationSignal::QualityWarning { .. } => {
            // check_token_threshold only returns None / TokenWarning /
            // HandoffTriggered; other variants cannot appear here.
        }
    }

    if let Some(errs) = latest_errors {
        out.extend(observe_failure_iteration(
            state,
            errs,
            config.failure_repeat_window,
        ));
    }

    if let Some(text) = latest_text
        && let IterationSignal::QualityWarning { signals } =
            check_quality_signals(text, &config.hedging_patterns)
    {
        out.push(IterationSignal::QualityWarning { signals });
    }

    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    fn config() -> IterationMonitorConfig {
        IterationMonitorConfig {
            context_window_tokens: 1000,
            warn_threshold_pct: 0.8,
            handoff_threshold_pct: 0.95,
            handoff_guidance: "Wrap up and prepare for handoff.".to_string(),
            failure_repeat_window: 3,
            hedging_patterns: vec!["I think maybe".to_string()],
        }
    }

    fn usage_with(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    // --- check_token_threshold ---------------------------------------

    #[test]
    fn token_threshold_below_warn_is_none() {
        let cfg = config();
        let u = usage_with(300, 200); // 50%
        match check_token_threshold(&u, &cfg) {
            IterationSignal::None => {}
            other => panic!("expected None, got {other:?}"),
        }
    }

    #[test]
    fn token_threshold_at_warn_is_token_warning() {
        let cfg = config();
        let u = usage_with(500, 350); // 85%
        match check_token_threshold(&u, &cfg) {
            IterationSignal::TokenWarning { used, limit, pct } => {
                assert_eq!(used, 850);
                assert_eq!(limit, 1000);
                assert!((pct - 0.85).abs() < f64::EPSILON);
            }
            other => panic!("expected TokenWarning, got {other:?}"),
        }
    }

    #[test]
    fn token_threshold_above_handoff_is_handoff_only() {
        let cfg = config();
        let u = usage_with(600, 360); // 96%
        match check_token_threshold(&u, &cfg) {
            IterationSignal::HandoffTriggered {
                used,
                limit,
                guidance,
            } => {
                assert_eq!(used, 960);
                assert_eq!(limit, 1000);
                assert_eq!(guidance, cfg.handoff_guidance);
            }
            other => panic!("expected HandoffTriggered, got {other:?}"),
        }
    }

    #[test]
    fn token_threshold_zero_usage_is_none() {
        let cfg = config();
        let u = usage_with(0, 0);
        match check_token_threshold(&u, &cfg) {
            IterationSignal::None => {}
            other => panic!("expected None, got {other:?}"),
        }
    }

    #[test]
    fn token_threshold_zero_window_is_none() {
        let mut cfg = config();
        cfg.context_window_tokens = 0;
        let u = usage_with(1_000_000, 1_000_000);
        match check_token_threshold(&u, &cfg) {
            IterationSignal::None => {}
            other => panic!("expected None for zero window, got {other:?}"),
        }
    }

    #[test]
    fn token_threshold_cache_tokens_not_counted() {
        let cfg = config();
        let u = Usage {
            input_tokens: 100,
            output_tokens: 100,
            cache_read_tokens: 5_000_000,
            cache_write_tokens: 5_000_000,
            cost_usd: None,
        };
        match check_token_threshold(&u, &cfg) {
            IterationSignal::None => {}
            other => panic!("expected None — cache tokens must not count, got {other:?}"),
        }
    }

    // --- format_handoff_message --------------------------------------

    #[test]
    fn handoff_message_contains_guidance() {
        let signal = IterationSignal::HandoffTriggered {
            used: 960,
            limit: 1000,
            guidance: "Wrap up cleanly.".to_string(),
        };
        let msg = format_handoff_message(&signal);
        assert!(msg.contains("Wrap up cleanly."));
    }

    #[test]
    fn handoff_message_contains_percentage() {
        let signal = IterationSignal::HandoffTriggered {
            used: 960,
            limit: 1000,
            guidance: "guidance".to_string(),
        };
        let msg = format_handoff_message(&signal);
        assert!(msg.contains("96.0%"), "expected 96.0% in {msg}");
    }

    #[test]
    fn handoff_message_contains_summarize_and_continuation() {
        let signal = IterationSignal::HandoffTriggered {
            used: 950,
            limit: 1000,
            guidance: "g".to_string(),
        };
        let msg = format_handoff_message(&signal);
        assert!(msg.contains("summarize"), "missing 'summarize' in {msg}");
        assert!(
            msg.contains("continuation"),
            "missing 'continuation' in {msg}",
        );
    }

    #[test]
    fn handoff_message_empty_for_other_variants() {
        assert!(format_handoff_message(&IterationSignal::None).is_empty());
        assert!(
            format_handoff_message(&IterationSignal::TokenWarning {
                used: 1,
                limit: 2,
                pct: 0.5,
            })
            .is_empty()
        );
    }

    #[test]
    fn handoff_message_handles_zero_limit() {
        let signal = IterationSignal::HandoffTriggered {
            used: 0,
            limit: 0,
            guidance: "g".to_string(),
        };
        let msg = format_handoff_message(&signal);
        assert!(msg.contains("0.0%"));
    }

    // --- observe_failure_iteration ------------------------------------

    /// Run `observe_failure_iteration` once per error, treating each
    /// entry as its own iteration's single-error batch.
    fn observe_each_as_iteration(
        state: &mut IterationMonitorState,
        errors: &[&str],
        window: usize,
    ) -> Vec<IterationSignal> {
        let mut out = Vec::new();
        for err in errors {
            out.extend(observe_failure_iteration(
                state,
                &[(*err).to_string()],
                window,
            ));
        }
        out
    }

    #[test]
    fn repeated_three_identical_iterations_window_three_triggers() {
        let mut state = IterationMonitorState::default();
        let out = observe_each_as_iteration(
            &mut state,
            &[
                "Error at line 5: foo",
                "Error at line 12: foo",
                "Error at line 99: foo",
            ],
            3,
        );
        match out.as_slice() {
            [
                IterationSignal::RepeatedFailure {
                    error_signature,
                    consecutive_count,
                },
            ] => {
                assert_eq!(*consecutive_count, 3);
                assert_eq!(error_signature, "error at line : foo");
            }
            other => panic!("expected one RepeatedFailure, got {other:?}"),
        }
    }

    #[test]
    fn repeated_three_identical_iterations_window_four_returns_none() {
        let mut state = IterationMonitorState::default();
        let out = observe_each_as_iteration(&mut state, &["e", "e", "e"], 4);
        assert!(out.is_empty(), "expected no signal, got {out:?}");
    }

    #[test]
    fn repeated_digit_stripping_normalizes_line_numbers() {
        // Same after stripping digits.
        let a = "Error at line 5: foo";
        let b = "Error at line 12: foo";
        assert_eq!(normalize_error(a), normalize_error(b));
    }

    #[test]
    fn repeated_empty_batch_resets_streaks() {
        let mut state = IterationMonitorState::default();
        observe_each_as_iteration(&mut state, &["e", "e"], 3);
        let cleared = observe_failure_iteration(&mut state, &[], 3);
        assert!(cleared.is_empty());
        // After the reset, two more failing iterations are below the window.
        let out = observe_each_as_iteration(&mut state, &["e", "e"], 3);
        assert!(
            out.is_empty(),
            "streaks must reset on a clean iteration: {out:?}"
        );
    }

    #[test]
    fn repeated_zero_window_returns_none() {
        let mut state = IterationMonitorState::default();
        let out = observe_each_as_iteration(&mut state, &["e", "e", "e", "e"], 0);
        assert!(out.is_empty(), "window 0 disables detection: {out:?}");
    }

    #[test]
    fn repeated_mixed_signatures_do_not_chain() {
        let mut state = IterationMonitorState::default();
        let out = observe_each_as_iteration(
            &mut state,
            &[
                "compile error: missing semicolon",
                "tool error: file not found",
                "schema error: missing field",
            ],
            3,
        );
        assert!(
            out.is_empty(),
            "distinct signatures must not chain: {out:?}"
        );
    }

    #[test]
    fn repeated_streak_can_start_after_unrelated_failures() {
        let mut state = IterationMonitorState::default();
        let out = observe_each_as_iteration(
            &mut state,
            &[
                "different",
                "also different",
                "Error at line 5: foo",
                "Error at line 12: foo",
                "Error at line 99: foo",
            ],
            3,
        );
        match out.as_slice() {
            [
                IterationSignal::RepeatedFailure {
                    consecutive_count, ..
                },
            ] => {
                assert_eq!(*consecutive_count, 3);
            }
            other => panic!("expected one RepeatedFailure, got {other:?}"),
        }
    }

    #[test]
    fn repeated_multi_error_iterations_track_each_signature() {
        // Both signatures recur across iterations; both trip on the
        // second iteration, in deterministic (sorted) order.
        let mut state = IterationMonitorState::default();
        let batch = vec!["error alpha".to_string(), "error beta".to_string()];
        let first = observe_failure_iteration(&mut state, &batch, 2);
        assert!(first.is_empty());
        let second = observe_failure_iteration(&mut state, &batch, 2);
        let signatures: Vec<&str> = second
            .iter()
            .filter_map(|s| match s {
                IterationSignal::RepeatedFailure {
                    error_signature, ..
                } => Some(error_signature.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(signatures, vec!["error alpha", "error beta"]);
    }

    // --- check_quality_signals ---------------------------------------

    #[test]
    fn quality_hedging_match() {
        let text = "I think maybe this could work, but the path is unclear.";
        let patterns = vec!["I think maybe".to_string()];
        match check_quality_signals(text, &patterns) {
            IterationSignal::QualityWarning { signals } => {
                assert_eq!(signals.len(), 1);
                match &signals[0] {
                    QualitySignal::Hedging {
                        matched_pattern,
                        text_excerpt,
                    } => {
                        assert_eq!(matched_pattern, "I think maybe");
                        assert!(text_excerpt.contains("I think maybe"));
                    }
                    other => panic!("expected Hedging, got {other:?}"),
                }
            }
            other => panic!("expected QualityWarning, got {other:?}"),
        }
    }

    #[test]
    fn quality_premature_completion_short_text() {
        let text = "Let me know if you need more"; // 28 chars
        let patterns: Vec<String> = Vec::new();
        match check_quality_signals(text, &patterns) {
            IterationSignal::QualityWarning { signals } => {
                assert_eq!(signals.len(), 1);
                assert!(matches!(
                    &signals[0],
                    QualitySignal::PrematureCompletion { .. }
                ));
            }
            other => panic!("expected QualityWarning, got {other:?}"),
        }
    }

    #[test]
    fn quality_clean_text_no_signal() {
        let text = "The function returns 42.";
        let patterns: Vec<String> = Vec::new();
        match check_quality_signals(text, &patterns) {
            IterationSignal::None => {}
            other => panic!("expected None, got {other:?}"),
        }
    }

    #[test]
    fn quality_multiple_patterns_match() {
        let text = "I think maybe and possibly this could work somehow.";
        let patterns = vec!["I think maybe".to_string(), "possibly".to_string()];
        match check_quality_signals(text, &patterns) {
            IterationSignal::QualityWarning { signals } => {
                assert_eq!(signals.len(), 2);
                assert!(
                    signals
                        .iter()
                        .all(|s| matches!(s, QualitySignal::Hedging { .. }))
                );
            }
            other => panic!("expected QualityWarning, got {other:?}"),
        }
    }

    #[test]
    fn quality_long_text_with_terminal_phrase_no_premature() {
        // 71+ chars: terminal phrase present but text is "long" so premature
        // completion does not trigger.
        let text =
            "Let me know if you have any further questions about the implementation details.";
        assert!(text.chars().count() >= PREMATURE_COMPLETION_CHAR_LIMIT);
        let patterns: Vec<String> = Vec::new();
        match check_quality_signals(text, &patterns) {
            IterationSignal::None => {}
            other => panic!("expected None, got {other:?}"),
        }
    }

    #[test]
    fn quality_empty_hedging_only_premature_checked() {
        let short_with_phrase = "I hope this helps!";
        match check_quality_signals(short_with_phrase, &[]) {
            IterationSignal::QualityWarning { signals } => {
                assert_eq!(signals.len(), 1);
                assert!(matches!(
                    &signals[0],
                    QualitySignal::PrematureCompletion { .. }
                ));
            }
            other => panic!("expected QualityWarning, got {other:?}"),
        }
    }

    #[test]
    fn quality_invalid_regex_is_skipped() {
        // `[` is an unterminated character class — invalid regex.
        let text = "I think maybe this fails";
        let patterns = vec!["[".to_string(), "I think maybe".to_string()];
        match check_quality_signals(text, &patterns) {
            IterationSignal::QualityWarning { signals } => {
                // Only the valid pattern produces a signal.
                assert_eq!(signals.len(), 1);
                match &signals[0] {
                    QualitySignal::Hedging {
                        matched_pattern, ..
                    } => assert_eq!(matched_pattern, "I think maybe"),
                    other => panic!("expected Hedging, got {other:?}"),
                }
            }
            other => panic!("expected QualityWarning, got {other:?}"),
        }
    }

    #[test]
    fn quality_excerpt_handles_multibyte_utf8() {
        // "café maybe résumé" — multibyte chars on both sides of the match.
        let text = "café maybe résumé";
        let patterns = vec!["maybe".to_string()];
        match check_quality_signals(text, &patterns) {
            IterationSignal::QualityWarning { signals } => {
                assert_eq!(signals.len(), 1);
                match &signals[0] {
                    QualitySignal::Hedging { text_excerpt, .. } => {
                        // Must not panic and excerpt must be the full text
                        // (it is short enough to fit in the window).
                        assert!(text_excerpt.contains("maybe"));
                        assert!(text_excerpt.contains("café"));
                        assert!(text_excerpt.contains("résumé"));
                    }
                    other => panic!("expected Hedging, got {other:?}"),
                }
            }
            other => panic!("expected QualityWarning, got {other:?}"),
        }
    }

    #[test]
    fn quality_premature_case_insensitive() {
        // Lowercased terminal phrase still matches.
        let text = "i hope this helps!";
        match check_quality_signals(text, &[]) {
            IterationSignal::QualityWarning { signals } => {
                assert_eq!(signals.len(), 1);
                assert!(matches!(
                    &signals[0],
                    QualitySignal::PrematureCompletion { .. }
                ));
            }
            other => panic!("expected QualityWarning, got {other:?}"),
        }
    }

    // --- evaluate_iteration ------------------------------------------

    #[test]
    fn evaluate_no_triggers_returns_empty() {
        let cfg = config();
        let mut state = IterationMonitorState::default();
        let u = usage_with(100, 100);
        let out = evaluate_iteration(
            &mut state,
            &u,
            Some("The function returns 42."),
            Some(&[]),
            &cfg,
        );
        assert!(out.is_empty(), "expected empty, got {out:?}");
    }

    #[test]
    fn evaluate_combines_warn_and_quality() {
        let cfg = config();
        let mut state = IterationMonitorState::default();
        let u = usage_with(500, 350); // 85% — TokenWarning
        let out = evaluate_iteration(
            &mut state,
            &u,
            Some("I think maybe this could work."),
            None,
            &cfg,
        );
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .any(|s| matches!(s, IterationSignal::TokenWarning { .. }))
        );
        assert!(
            out.iter()
                .any(|s| matches!(s, IterationSignal::QualityWarning { .. }))
        );
    }

    #[test]
    fn evaluate_handoff_fires_once_only() {
        let cfg = config();
        let mut state = IterationMonitorState::default();
        let u = usage_with(600, 360); // 96% — HandoffTriggered

        let first = evaluate_iteration(&mut state, &u, None, None, &cfg);
        assert!(
            first
                .iter()
                .any(|s| matches!(s, IterationSignal::HandoffTriggered { .. })),
        );
        assert!(state.handoff_fired);

        let second = evaluate_iteration(&mut state, &u, None, None, &cfg);
        assert!(
            !second
                .iter()
                .any(|s| matches!(s, IterationSignal::HandoffTriggered { .. })),
            "handoff fired twice: {second:?}",
        );
    }

    #[test]
    fn evaluate_warn_fires_once_only() {
        let cfg = config();
        let mut state = IterationMonitorState::default();
        let u = usage_with(500, 350); // 85% — TokenWarning

        let first = evaluate_iteration(&mut state, &u, None, None, &cfg);
        assert!(
            first
                .iter()
                .any(|s| matches!(s, IterationSignal::TokenWarning { .. })),
        );
        assert!(state.warn_fired);

        let second = evaluate_iteration(&mut state, &u, None, None, &cfg);
        assert!(
            !second
                .iter()
                .any(|s| matches!(s, IterationSignal::TokenWarning { .. })),
            "warn fired twice: {second:?}",
        );
    }

    #[test]
    fn evaluate_tracks_failing_iterations_across_calls() {
        let cfg = config(); // failure_repeat_window: 3
        let mut state = IterationMonitorState::default();
        let u = usage_with(0, 0);

        evaluate_iteration(
            &mut state,
            &u,
            None,
            Some(&["Error at line 5: foo".to_string()]),
            &cfg,
        );
        evaluate_iteration(
            &mut state,
            &u,
            None,
            Some(&["Error at line 12: foo".to_string()]),
            &cfg,
        );
        let out = evaluate_iteration(
            &mut state,
            &u,
            None,
            Some(&["Error at line 99: foo".to_string()]),
            &cfg,
        );

        let triggered = out
            .iter()
            .any(|s| matches!(s, IterationSignal::RepeatedFailure { .. }));
        assert!(triggered, "expected RepeatedFailure in {out:?}");
    }

    #[test]
    fn evaluate_token_warning_independent_of_handoff() {
        // After handoff has fired, a subsequent step that drops back into
        // the warn band (between warn and handoff) still emits TokenWarning
        // because warn_fired is independent of handoff_fired.
        let cfg = config();
        let mut state = IterationMonitorState {
            handoff_fired: true,
            ..IterationMonitorState::default()
        };
        let u = usage_with(500, 350); // 85% — TokenWarning band
        let out = evaluate_iteration(&mut state, &u, None, None, &cfg);
        assert!(
            out.iter()
                .any(|s| matches!(s, IterationSignal::TokenWarning { .. })),
            "expected TokenWarning to fire independently, got {out:?}",
        );
    }

    // --- Fix campaign Track L, finding 3: RepeatedFailure semantics -------

    /// One iteration that produces `window` identical errors in a single
    /// batch is one failed attempt, not a loop: the signal must not fire.
    #[test]
    fn single_iteration_error_batch_does_not_trip() {
        let mut cfg = config();
        cfg.failure_repeat_window = 3;
        let mut state = IterationMonitorState::default();
        let u = usage_with(0, 0);

        let batch = vec![
            "Error at line 5: foo".to_string(),
            "Error at line 12: foo".to_string(),
            "Error at line 99: foo".to_string(),
        ];
        let out = evaluate_iteration(&mut state, &u, None, Some(&batch), &cfg);
        assert!(
            !out.iter()
                .any(|s| matches!(s, IterationSignal::RepeatedFailure { .. })),
            "a single batch of identical errors is one attempt, not a loop: {out:?}",
        );
    }

    /// After the signal trips, it must not re-fire on every subsequent
    /// failing iteration: a re-fire requires another full window of new
    /// consecutive failing iterations.
    #[test]
    fn sustained_failures_do_not_refire_every_iteration() {
        let mut cfg = config();
        cfg.failure_repeat_window = 2;
        let mut state = IterationMonitorState::default();
        let u = usage_with(0, 0);
        let errs = ["same failure".to_string()];

        let fired = |out: &[IterationSignal]| {
            out.iter()
                .any(|s| matches!(s, IterationSignal::RepeatedFailure { .. }))
        };

        let i1 = evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        assert!(!fired(&i1), "one failing iteration is below the window");
        let i2 = evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        assert!(fired(&i2), "window reached: signal fires");
        let i3 = evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        assert!(
            !fired(&i3),
            "must not re-fire one iteration after the last fire: {i3:?}",
        );
        let i4 = evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        assert!(
            fired(&i4),
            "a full new window of failures accumulated: re-fire expected",
        );
        if let Some(IterationSignal::RepeatedFailure {
            consecutive_count, ..
        }) = i4
            .iter()
            .find(|s| matches!(s, IterationSignal::RepeatedFailure { .. }))
        {
            assert_eq!(
                *consecutive_count, 4,
                "count reports the full consecutive streak",
            );
        }
    }

    /// An error-free iteration breaks every streak; a relapse must again
    /// accumulate a full window before the signal fires.
    #[test]
    fn recovery_then_new_failures_requires_full_window() {
        let mut cfg = config();
        cfg.failure_repeat_window = 2;
        let mut state = IterationMonitorState::default();
        let u = usage_with(0, 0);
        let errs = ["same failure".to_string()];
        let fired = |out: &[IterationSignal]| {
            out.iter()
                .any(|s| matches!(s, IterationSignal::RepeatedFailure { .. }))
        };

        evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        let trip = evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        assert!(fired(&trip), "initial trip");

        // Clean iteration: explicit empty error batch = recovery.
        let clean = evaluate_iteration(&mut state, &u, None, Some(&[]), &cfg);
        assert!(!fired(&clean));

        let r1 = evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        assert!(
            !fired(&r1),
            "after recovery one failing iteration is below the window: {r1:?}",
        );
        let r2 = evaluate_iteration(&mut state, &u, None, Some(&errs), &cfg);
        assert!(fired(&r2), "relapse reached a full window: fires again");
    }

    /// A signature that skips an iteration (different error in between)
    /// loses its streak: matching is over consecutive iterations.
    #[test]
    fn interleaved_different_error_breaks_the_streak() {
        let mut cfg = config();
        cfg.failure_repeat_window = 2;
        let mut state = IterationMonitorState::default();
        let u = usage_with(0, 0);
        let fired = |out: &[IterationSignal]| {
            out.iter()
                .any(|s| matches!(s, IterationSignal::RepeatedFailure { .. }))
        };

        let a = ["error alpha".to_string()];
        let b = ["error beta".to_string()];
        evaluate_iteration(&mut state, &u, None, Some(&a), &cfg);
        let out = evaluate_iteration(&mut state, &u, None, Some(&b), &cfg);
        assert!(!fired(&out), "different signatures must not chain: {out:?}");
        let out = evaluate_iteration(&mut state, &u, None, Some(&a), &cfg);
        assert!(
            !fired(&out),
            "alpha's streak was broken by the beta iteration: {out:?}",
        );
    }

    #[test]
    fn evaluate_iteration_uses_failure_window_from_config() {
        let mut cfg = config();
        cfg.failure_repeat_window = 4;
        let mut state = IterationMonitorState::default();
        let u = usage_with(0, 0);

        // Three identical errors: with window=4 this should NOT trigger.
        for _ in 0..3 {
            evaluate_iteration(&mut state, &u, None, Some(&["same".to_string()]), &cfg);
        }
        let out = evaluate_iteration(&mut state, &u, None, None, &cfg);
        assert!(
            !out.iter()
                .any(|s| matches!(s, IterationSignal::RepeatedFailure { .. })),
            "should not trigger with window=4 and only 3 errors: {out:?}",
        );

        // Add a fourth identical error — now it should trigger.
        let out2 = evaluate_iteration(&mut state, &u, None, Some(&["same".to_string()]), &cfg);
        assert!(
            out2.iter()
                .any(|s| matches!(s, IterationSignal::RepeatedFailure { .. })),
            "expected RepeatedFailure after 4 matching errors: {out2:?}",
        );
    }
}
