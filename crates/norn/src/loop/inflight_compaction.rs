//! Pre-call context preflight: token estimation, the auto-compaction
//! trigger, and in-flight application of a fired compaction (REVIEW
//! item 6b).
//!
//! Before each provider call the runner estimates the prompt size, emits a
//! `loop.token_warning` event when `max(estimate, usage_floor)` exceeds the
//! configured context-window limit, and fires the auto-compaction trigger
//! when that same effective count crosses the reserve threshold (the floor
//! is the previous provider call's reported spend — see
//! [`ContextEdits::usage_floor`]). Historically the trigger only marked
//! events superseded in the [`ContextEdits`] tracker, which changed what
//! the *next* step's prompt would contain — the in-flight request had
//! already been built from the uncompacted message list. This module closes
//! that gap: when compaction fires, the live message list is rewritten so
//! the current request is built from the compacted view.
//!
//! # Mapping invariant
//!
//! Every message in the live conversation past the prefix (the System
//! message) corresponds 1:1, in order, to a visible prompt-producing session
//! event — with a single exception: the step's persisted user-prompt event
//! may expand to several messages when a slash command was spliced in.
//! [`InFlightPromptLayout`] carries that event's ID and expansion width so
//! the walk in [`apply_compaction_in_flight`] can account for it. The
//! invariant holds because every loop-side message push (assistant turns,
//! tool results, nudges, inbound injections, rule injections, child-agent
//! results, stop blocks) is paired with exactly one persisted
//! prompt-producing event, and events that produce no message (Custom, Label,
//! forks, …) are never pushed locally. The managed dynamic-context Developer
//! message is *not* in the live conversation while this walk runs: the
//! request builder detaches it before the preflight and re-attaches it at the
//! tail afterwards (its token cost is fed in separately via
//! [`PreflightArgs::dev_tail_tokens`]), so it never perturbs the mapping.

use std::collections::HashSet;

use crate::error::SessionError;
use crate::r#loop::compaction::{
    AutoCompactArgs, CompactionState, CompactionSummarySource, maybe_auto_compact,
};
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::conversation_state::{ConversationRequestState, event_produces_prompt_message};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::tokens::estimate_prompt_tokens;
use crate::provider::agent_event::{
    AgentCompaction, AgentEventSender, COMPACTION_EVENT_TYPE, CompactionSummaryKind,
};
use crate::provider::request::{Message, ToolDefinition};
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::context_edit::{AutoCompactionOutcome, ContextEdits};
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::store::EventStore;

use super::helpers::append_and_notify;

/// Positional facts about the live conversation needed to map session
/// events onto message indices (see module docs for the invariant).
pub(super) struct InFlightPromptLayout {
    /// Number of leading non-event messages: the System message. The managed
    /// dynamic-context Developer message is detached before the preflight, so
    /// it is not counted here.
    pub(super) prefix_len: usize,
    /// Event ID of this step's persisted user-prompt event.
    pub(super) prompt_event_id: EventId,
    /// Number of local messages the prompt event corresponds to: 1 for a
    /// literal prompt, N for a slash-command expansion.
    pub(super) prompt_message_len: usize,
}

/// Borrowed loop state consumed by [`run_context_preflight`].
pub(super) struct PreflightArgs<'a> {
    /// Session event store.
    pub(super) store: &'a EventStore,
    /// The step's provider; issues the compaction-summarization call.
    pub(super) provider: &'a dyn Provider,
    /// The step's resolved model, reused for summarization.
    pub(super) model: &'a str,
    /// Live conversation; rewritten in place when compaction fires.
    pub(super) messages: &'a mut Vec<Message>,
    /// Tool definitions accompanying this request (counted in the estimate).
    pub(super) iteration_tools: &'a [ToolDefinition],
    /// Provider-state cursor tracker, notified of message removals.
    pub(super) conversation_state: &'a mut ConversationRequestState,
    /// Loop context holding the estimator, hooks, and context edits.
    pub(super) loop_context: &'a mut LoopContext,
    /// Step configuration (window limit, threshold, keep-turns).
    pub(super) config: &'a AgentLoopConfig,
    /// Once-per-step compaction guard.
    pub(super) compaction_state: &'a mut CompactionState,
    /// Positional layout of the live conversation.
    pub(super) layout: InFlightPromptLayout,
    /// Estimated token cost of the managed dynamic-context Developer message
    /// that the request builder attaches at the tail *after* this preflight.
    /// It is not present in `messages` while the estimate and compaction walk
    /// run (see the mapping invariant), so its cost is added in explicitly:
    /// the token warning and the auto-compaction trigger must account for the
    /// message that actually goes over the wire.
    pub(super) dev_tail_tokens: usize,
    /// The step's cooperative cancellation token, raced against the
    /// compaction-summarization provider call.
    pub(super) cancel: Option<&'a tokio_util::sync::CancellationToken>,
    /// Live agent-event channel. When present and a compaction fires, a
    /// [`AgentCompaction`] event is broadcast so embedders can surface the
    /// history rewrite and account for the summarization spend.
    pub(super) event_tx: Option<&'a AgentEventSender>,
}

/// What the preflight did, reported back to the runner.
#[derive(Debug, Default)]
pub(super) struct PreflightOutcome {
    /// Estimated prompt/input tokens for the provider request that will
    /// be sent after preflight. `None` means no estimator is configured.
    pub(super) request_input_estimate: Option<usize>,
    /// Usage of the compaction-summarization provider call, when one was
    /// issued. The runner must fold this into the step's accumulated
    /// usage exactly like any other provider call.
    pub(super) summarization_usage: Option<Usage>,
}

/// Run token estimation, the token-warning event, the auto-compaction
/// trigger, and in-flight compaction application.
///
/// The estimate is computed over the **full live conversation**, not the
/// threaded request delta: with provider-side response threading the server
/// reconstructs the entire history from `previous_response_id`, so the delta
/// drastically understates the real context size. Threaded state is compacted
/// by the provider and never by the local summarizer; stateless replay retains
/// the local compaction path below.
///
/// The caller must build its request message list *after* this returns.
/// With no token estimator configured this is a no-op.
///
/// # Errors
///
/// Propagates [`SessionError`] from event appends and from
/// [`maybe_auto_compact`].
pub(super) async fn run_context_preflight(
    args: PreflightArgs<'_>,
) -> Result<PreflightOutcome, SessionError> {
    let Some(estimator) = args.loop_context.token_estimator.clone() else {
        return Ok(PreflightOutcome::default());
    };

    // R3: client-side token estimation runs immediately before the provider
    // call. Advisory only — the call still proceeds. Both the warning and
    // the compaction trigger below anchor on `max(estimate, usage_floor)`:
    // the character estimate cannot see request content the provider
    // re-bills every call (replayed encrypted reasoning items on stateless
    // Responses backends), while the last call's reported spend is a
    // truthful lower bound for a request that has only grown since.
    // `ContextEdits` clears the floor whenever the prompt view shrinks.
    let estimated = estimate_prompt_tokens(estimator.as_ref(), args.messages, args.iteration_tools)
        + args.dev_tail_tokens;
    let usage_floor = args
        .loop_context
        .context_edits
        .as_ref()
        .and_then(ContextEdits::usage_floor);
    let effective = {
        let estimated = u64::try_from(estimated).unwrap_or(u64::MAX);
        usage_floor.map_or(estimated, |floor| estimated.max(floor))
    };
    let mut request_input_estimate = estimated;
    let hooks = args.loop_context.hooks.clone();
    if let Some(limit) = args.config.context_window_limit
        && effective > limit
    {
        append_and_notify(
            args.store,
            SessionEvent::Custom {
                base: EventBase::new(args.store.last_event_id()),
                event_type: "loop.token_warning".to_string(),
                data: serde_json::json!({
                    "estimated": estimated,
                    "usage_floor": usage_floor,
                    "effective": effective,
                    "limit": limit,
                }),
            },
            hooks.as_deref(),
        )
        .await?;
    }

    // D3: provider-owned threads and local prompt summaries are mutually
    // exclusive state strategies. The estimate and warning above still use the
    // full reconstructed context, but a threaded request relies on its
    // `context_management` contract and must not reset the anchor to replay a
    // locally rewritten view whose stored reasoning may be unavailable.
    if args.conversation_state.store() {
        return Ok(PreflightOutcome {
            request_input_estimate: Some(estimated),
            summarization_usage: None,
        });
    }

    // R4: auto-compaction trigger fires once per step when
    // `max(estimate, usage_floor)` crosses
    // `context_window_limit − auto_compact_reserve_tokens`.
    // NH-006 R6: the CompactionHook chain runs inside `maybe_auto_compact`;
    // a Block returns Ok(None) and the trigger is skipped (logged inside).
    let run = maybe_auto_compact(AutoCompactArgs {
        state: args.compaction_state,
        edits: args.loop_context.context_edits.as_mut(),
        store: args.store,
        provider: args.provider,
        model: args.model,
        estimated_tokens: estimated,
        usage_floor,
        context_window_limit: args.config.context_window_limit,
        reserve_tokens: args.config.auto_compact_reserve_tokens,
        keep_recent_turns: args.config.auto_compact_keep_recent_turns,
        hooks: hooks.as_deref(),
        cancel: args.cancel,
    })
    .await?;
    let Some(run) = run else {
        return Ok(PreflightOutcome {
            request_input_estimate: Some(request_input_estimate),
            summarization_usage: None,
        });
    };
    tracing::debug!(
        freed_token_estimate = run.freed_token_estimate,
        newly_superseded = run.outcome.newly_superseded.len(),
        "auto-compaction suppressed older prompt context"
    );

    let summarization_usage = run.summarization_usage;

    if let Some(edits) = args.loop_context.context_edits.as_ref() {
        if apply_compaction_in_flight(
            args.store,
            edits,
            &run.outcome,
            &args.layout,
            args.messages,
            args.conversation_state,
        ) {
            request_input_estimate =
                estimate_prompt_tokens(estimator.as_ref(), args.messages, args.iteration_tools)
                    + args.dev_tail_tokens;
        }
    } else {
        // Unreachable by construction: `maybe_auto_compact` only fires when
        // a ContextEdits tracker is present. Guarded rather than unwrapped —
        // and NOT an early return: the compaction is already committed to
        // the store, so the audit record and live broadcast below must
        // still fire. The in-flight rewrite simply could not be evaluated,
        // which the accounting reports as `tokens_after == tokens_before`.
        tracing::error!("compaction fired without a ContextEdits tracker");
    }

    // The pre-/post-rewrite accounting shared verbatim by the persisted
    // audit record and the live broadcast: `tokens_before` is the estimate
    // over the full conversation, `tokens_after` reflects the compacted
    // view (equal to `tokens_before` when the in-flight rewrite could not
    // be applied — the compaction still takes effect next step).
    let tokens_before = u64::try_from(estimated).unwrap_or(u64::MAX);
    let tokens_after = u64::try_from(request_input_estimate).unwrap_or(u64::MAX);
    let events_compacted = run.outcome.newly_superseded.len();

    // Persist the audit record of the compaction and its summarization
    // call so the outcome, cost, and accounting are visible at the session
    // level, not only on the step's returned usage total. Deliberately
    // carries the same facts as the live `AgentCompaction` broadcast below
    // — `compaction_id`, `events_compacted`, `tokens_before`,
    // `tokens_after` included — so a log-only consumer reproduces the live
    // event's accounting exactly (session-fidelity Gap 9).
    let (summary_kind, summarization_error) = match &run.summary_source {
        CompactionSummarySource::Llm => ("llm_summary", None),
        CompactionSummarySource::MechanicalDigestFallback { error } => {
            ("mechanical_digest_fallback", Some(error.clone()))
        }
    };
    append_and_notify(
        args.store,
        SessionEvent::Custom {
            base: EventBase::new(args.store.last_event_id()),
            event_type: COMPACTION_EVENT_TYPE.to_string(),
            data: serde_json::json!({
                "summary_kind": summary_kind,
                "summarization_error": summarization_error,
                "model": args.model,
                "freed_token_estimate": run.freed_token_estimate,
                "compaction_id": run.outcome.compaction_id.to_string(),
                "events_compacted": events_compacted,
                "tokens_before": tokens_before,
                "tokens_after": tokens_after,
                "usage": summarization_usage.as_ref().map(|u| serde_json::json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                    "cache_read_tokens": u.cache_read_tokens,
                    "cache_write_tokens": u.cache_write_tokens,
                    "cost_usd": u.cost_usd,
                })),
            }),
        },
        hooks.as_deref(),
    )
    .await?;

    // C4: broadcast the completed compaction live so an embedder can surface
    // the history rewrite and fold the summarization spend into its
    // accounting — the twin of the `loop.compaction_summarization` audit
    // record appended above, carrying the identical facts.
    if let Some(tx) = args.event_tx {
        let summary_source = match &run.summary_source {
            CompactionSummarySource::Llm => CompactionSummaryKind::Llm,
            CompactionSummarySource::MechanicalDigestFallback { error } => {
                CompactionSummaryKind::MechanicalDigestFallback {
                    error: error.clone(),
                }
            }
        };
        tx.send_compaction(AgentCompaction {
            compaction_id: run.outcome.compaction_id.clone(),
            events_compacted,
            tokens_before,
            tokens_after,
            model: args.model.to_owned(),
            freed_token_estimate: run.freed_token_estimate,
            summary_source,
            summarization_usage: summarization_usage.clone(),
            compacted_at: chrono::Utc::now(),
        });
    }

    Ok(PreflightOutcome {
        request_input_estimate: Some(request_input_estimate),
        summarization_usage,
    })
}

/// Rewrite the live message list to reflect a just-fired compaction:
/// remove the messages whose backing events were newly superseded and
/// append the compaction-summary Developer message at the tail (matching
/// where the persisted compaction event sits in store order, so a later
/// replay from the store produces the same conversation).
///
/// Returns `true` when the rewrite was applied. When the event-to-message
/// mapping cannot be verified (cursor mismatch — a loop invariant
/// violation), the rewrite is skipped with a loud error log and `false` is
/// returned: the compaction remains persisted and takes effect on the next
/// step, exactly the pre-fix behaviour, and the in-flight request is built
/// from the unmodified (larger but complete) conversation.
pub(super) fn apply_compaction_in_flight(
    store: &EventStore,
    edits: &ContextEdits,
    outcome: &AutoCompactionOutcome,
    layout: &InFlightPromptLayout,
    messages: &mut Vec<Message>,
    conversation_state: &mut ConversationRequestState,
) -> bool {
    let newly: HashSet<&EventId> = outcome.newly_superseded.iter().collect();

    // Walk the store in order, replaying pre-compaction visibility, and
    // record the message spans backed by newly superseded events.
    let mut cursor = layout.prefix_len;
    let mut remove_spans: Vec<(usize, usize)> = Vec::new();
    for event in store.events() {
        let id = &event.base().id;
        if *id == outcome.compaction_id {
            // The compaction event itself was appended by this trigger and
            // has no local message yet.
            continue;
        }
        if edits.is_suppressed(id) {
            continue;
        }
        let newly_superseded = newly.contains(id);
        if edits.is_superseded(id) && !newly_superseded {
            // Hidden before this step's prompt was built — never had a
            // local message.
            continue;
        }
        if !event_produces_prompt_message(&event, true) {
            continue;
        }
        let span = if *id == layout.prompt_event_id {
            layout.prompt_message_len
        } else {
            1
        };
        if newly_superseded {
            remove_spans.push((cursor, span));
        }
        cursor += span;
    }

    if cursor != messages.len() {
        tracing::error!(
            expected = cursor,
            actual = messages.len(),
            prefix_len = layout.prefix_len,
            "in-flight compaction mapping mismatch; the compaction stays \
             persisted and takes effect on the next step instead",
        );
        return false;
    }

    // Remove from the highest index down so earlier spans stay valid.
    for (start, span) in remove_spans.iter().rev() {
        for idx in (*start..start + span).rev() {
            messages.remove(idx);
            conversation_state.note_removed_message(idx);
        }
    }

    // Render the summary message through the same projection used for
    // persisted history so in-flight and resumed conversations agree.
    let Some(compaction_event) = store.get(&outcome.compaction_id) else {
        tracing::error!(
            "compaction event missing from store immediately after append; \
             proceeding without an in-flight summary message",
        );
        return true;
    };
    messages.extend(crate::session::conversion::prompt_events_to_messages(&[
        compaction_event,
    ]));
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::r#loop::config::AgentLoopConfig;
    use crate::provider::request::MessageRole;
    use crate::provider::tools::ProviderCapabilities;
    use crate::session::events::EventUsage;

    fn user_event(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn assistant_event(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: None,
        }
    }

    fn message(role: MessageRole, content: &str) -> Message {
        Message {
            response_items: Vec::new(),
            role,
            content: Some(content.to_string()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        }
    }

    fn state(prefix_len: usize) -> ConversationRequestState {
        ConversationRequestState::new(
            &AgentLoopConfig::default(),
            ProviderCapabilities::default(),
            prefix_len,
            None,
        )
        .expect("state")
    }

    /// Build a store with `pairs` user/assistant turns, fire an
    /// auto-compaction keeping one turn, and return the pieces.
    fn compacted_store(
        pairs: usize,
        keep: usize,
    ) -> (EventStore, ContextEdits, AutoCompactionOutcome) {
        let store = EventStore::new();
        for i in 0..pairs {
            store.append(user_event(&format!("q{i}"))).expect("append");
            store
                .append(assistant_event(&format!("a{i}")))
                .expect("append");
        }
        let mut edits = ContextEdits::new();
        let outcome = edits
            .auto_compact_keeping_recent_turns(&store, keep, 100)
            .expect("compaction runs")
            .expect("compaction fires");
        (store, edits, outcome)
    }

    #[test]
    fn removes_superseded_messages_and_appends_summary() {
        let (store, edits, outcome) = compacted_store(3, 1);
        // Local conversation mirrors the store: prefix [System] + 6 history
        // messages; the last user message doubles as the prompt event.
        let prompt_event_id = store.events()[4].base().id.clone();
        let mut messages = vec![message(MessageRole::System, "system")];
        for i in 0..3 {
            messages.push(message(MessageRole::User, &format!("q{i}")));
            messages.push(message(MessageRole::Assistant, &format!("a{i}")));
        }
        let mut cs = state(1);
        let layout = InFlightPromptLayout {
            prefix_len: 1,
            prompt_event_id,
            prompt_message_len: 1,
        };

        let applied =
            apply_compaction_in_flight(&store, &edits, &outcome, &layout, &mut messages, &mut cs);

        assert!(applied);
        // keep_turns=1 keeps the final assistant turn and the user message
        // between the prior assistant turn and it; q0/a0/q1/a1 are gone.
        let contents: Vec<String> = messages.iter().filter_map(|m| m.content.clone()).collect();
        assert!(
            !contents.iter().any(|c| c == "q0" || c == "a0" || c == "a1"),
            "superseded turns must be removed: {contents:?}",
        );
        assert!(
            contents.iter().any(|c| c == "a2"),
            "kept turn must survive: {contents:?}",
        );
        let last = messages.last().expect("summary appended");
        assert_eq!(last.role, MessageRole::Developer);
        assert!(
            last.content
                .as_deref()
                .is_some_and(|c| c.contains("compaction summary")),
            "summary message must be the rendered compaction event",
        );
    }

    #[test]
    fn slash_expansion_span_is_removed_as_a_unit() {
        let store = EventStore::new();
        // Old prompt event that expanded to 2 local messages, then enough
        // assistant turns that the expansion span gets compacted away.
        let prompt_id = store.append(user_event("/cmd")).expect("append");
        for i in 0..4 {
            store
                .append(assistant_event(&format!("a{i}")))
                .expect("append");
        }
        let mut edits = ContextEdits::new();
        let outcome = edits
            .auto_compact_keeping_recent_turns(&store, 1, 50)
            .expect("compaction runs")
            .expect("compaction fires");
        assert!(
            outcome.newly_superseded.contains(&prompt_id),
            "test setup: the expansion-backed event must be compacted",
        );

        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "expanded part 1"),
            message(MessageRole::User, "expanded part 2"),
            message(MessageRole::Assistant, "a0"),
            message(MessageRole::Assistant, "a1"),
            message(MessageRole::Assistant, "a2"),
            message(MessageRole::Assistant, "a3"),
        ];
        let mut cs = state(1);
        let layout = InFlightPromptLayout {
            prefix_len: 1,
            prompt_event_id: prompt_id,
            prompt_message_len: 2,
        };

        let applied =
            apply_compaction_in_flight(&store, &edits, &outcome, &layout, &mut messages, &mut cs);

        assert!(applied);
        let contents: Vec<String> = messages.iter().filter_map(|m| m.content.clone()).collect();
        assert!(
            !contents.iter().any(|c| c.starts_with("expanded part")),
            "both expansion messages must be removed: {contents:?}",
        );
        assert!(contents.iter().any(|c| c == "a3"), "kept: {contents:?}");
    }

    #[test]
    fn mapping_mismatch_degrades_without_mutating() {
        let (store, edits, outcome) = compacted_store(3, 1);
        let prompt_event_id = store.events()[4].base().id.clone();
        // Conversation deliberately shorter than the store view implies.
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "q0"),
        ];
        let before = messages.clone();
        let mut cs = state(1);
        let layout = InFlightPromptLayout {
            prefix_len: 1,
            prompt_event_id,
            prompt_message_len: 1,
        };

        let applied =
            apply_compaction_in_flight(&store, &edits, &outcome, &layout, &mut messages, &mut cs);

        assert!(!applied, "mismatch must not be applied");
        assert_eq!(
            messages.len(),
            before.len(),
            "conversation must be untouched on mismatch",
        );
    }

    #[test]
    fn previously_superseded_events_do_not_shift_the_mapping() {
        let store = EventStore::new();
        let old = store.append(user_event("ancient")).expect("append");
        let mut edits = ContextEdits::new();
        // Hidden before this step started: produces no local message.
        edits
            .summarize(&store, vec![old], "prior summary".to_owned())
            .expect("summarize");
        for i in 0..4 {
            store.append(user_event(&format!("q{i}"))).expect("append");
            store
                .append(assistant_event(&format!("a{i}")))
                .expect("append");
        }
        let outcome = edits
            .auto_compact_keeping_recent_turns(&store, 1, 50)
            .expect("compaction runs")
            .expect("compaction fires");

        // Local view at step start: prefix + prior-summary Developer message
        // (the earlier compaction event renders) + 8 turn messages.
        let prompt_event_id = store.events()[7].base().id.clone();
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Developer, "prior summary rendered"),
        ];
        for i in 0..4 {
            messages.push(message(MessageRole::User, &format!("q{i}")));
            messages.push(message(MessageRole::Assistant, &format!("a{i}")));
        }
        let mut cs = state(1);
        let layout = InFlightPromptLayout {
            prefix_len: 1,
            prompt_event_id,
            prompt_message_len: 1,
        };

        let applied =
            apply_compaction_in_flight(&store, &edits, &outcome, &layout, &mut messages, &mut cs);

        assert!(applied);
        let contents: Vec<String> = messages.iter().filter_map(|m| m.content.clone()).collect();
        assert!(
            !contents.iter().any(|c| c == "prior summary rendered"),
            "the folded-in prior summary must be removed: {contents:?}",
        );
        assert!(contents.iter().any(|c| c == "a3"));
    }
}
