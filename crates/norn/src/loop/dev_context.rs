//! Managed dynamic-context Developer message tracking.
//!
//! The agent loop maintains at most one Developer-role message that carries
//! the current dynamic context (environment section, collaboration mode,
//! rule-injected sections, prompt-command output). Its content changes every
//! iteration — most reliably the `# Environment` `Time:` field, at second
//! resolution — so it is re-synced fresh on every provider call.
//!
//! # Placement: the tail, not the prefix
//!
//! The message is placed at the **tail** of the live conversation — after the
//! System message, after all persisted history, after the new user input, and
//! after any prior-iteration tool results — so it is the last message before
//! the model responds. Placing it ahead of history (its former home at
//! `messages[1]`) meant its per-turn byte change invalidated the provider's
//! prefix cache for the *entire* growing history; at the tail, the System
//! message plus history form one stable, fully-cacheable prefix and only the
//! small trailing message changes each turn. See
//! `docs/PROMPT-CACHE-INVALIDATION-FIX.md`.
//!
//! # Lifecycle: detach before preflight, attach after
//!
//! [`ManagedDevMessage`] is consulted twice per iteration in
//! [`build_request`](crate::r#loop::runner::prompt):
//!
//! 1. [`Self::detach`] runs *before* the context preflight. It removes the
//!    message the previous iteration attached, restoring the invariant that
//!    every message past the System prefix corresponds 1:1 to a persisted
//!    prompt-producing event — the invariant the in-flight compaction walk
//!    relies on. The compaction summary (which is event-backed and lives in
//!    history permanently) is therefore the only Developer message present
//!    while the walk runs, and it is never a candidate for removal here.
//! 2. [`Self::attach`] runs *after* the preflight. It appends the freshly
//!    built message at the current tail, so it lands after any compaction
//!    summary the preflight appended and is the last message in the request.
//!
//! # Slot identity
//!
//! Between one iteration's `attach` and the next iteration's `detach`, every
//! conversation mutation the loop performs (assistant turns, tool results,
//! nudges, inbound/child injections) is a **tail append** — landing *after*
//! the managed message — so the managed message's index never shifts and the
//! stored index stays exactly on it. A Developer-role compaction summary sits
//! earlier, in history, and is never at the stored index. `detach` verifies
//! the stored index still holds a Developer message before removing it; a
//! mismatch is a loop-invariant violation, logged loudly, and self-healed by
//! forgetting the slot rather than clobbering whatever now occupies it.

use crate::r#loop::conversation_state::ConversationRequestState;
use crate::provider::request::{Message, MessageRole};

/// Tracker for the loop-managed dynamic-context Developer message.
///
/// Constructed once per step (initially absent — the first
/// [`build_request`](crate::r#loop::runner::prompt) attaches the message) and
/// driven through [`Self::detach`] / [`Self::attach`] every iteration.
#[derive(Debug)]
pub(super) struct ManagedDevMessage {
    /// Index of the managed message in the live conversation while it is
    /// attached; `None` between a `detach` and the next `attach`, and before
    /// the first `attach` of the step.
    index: Option<usize>,
}

impl ManagedDevMessage {
    /// Create a tracker for a step that has not yet placed the message.
    pub(super) const fn new() -> Self {
        Self { index: None }
    }

    /// Remove the message placed by the previous iteration.
    ///
    /// Restores the 1:1 message-to-event mapping past the System prefix so the
    /// preflight token estimate and in-flight compaction walk operate on a
    /// clean history. `conversation_state` is told about the removal so its
    /// threaded-delta cursor (`input_start`) tracks the messages that shift
    /// down into the removed slot.
    ///
    /// A no-op before the first attach. If the stored index no longer holds a
    /// Developer message — a loop-invariant violation, since only tail appends
    /// occur between attach and detach — the tracker logs loudly and forgets
    /// the slot without removing anything, so the next attach places a fresh
    /// message rather than this path clobbering an unrelated message.
    pub(super) fn detach(
        &mut self,
        messages: &mut Vec<Message>,
        conversation_state: &mut ConversationRequestState,
    ) {
        let Some(idx) = self.index else {
            return;
        };
        if messages
            .get(idx)
            .is_some_and(|m| matches!(m.role, MessageRole::Developer))
        {
            messages.remove(idx);
            conversation_state.note_removed_message(idx);
        } else {
            tracing::error!(
                index = idx,
                message_count = messages.len(),
                "managed developer slot no longer holds a Developer message; \
                 forgetting the slot (this indicates a loop invariant violation)",
            );
        }
        self.index = None;
    }

    /// Append the freshly built managed message at the tail.
    ///
    /// Called after the preflight with the current dynamic-context `content`
    /// (already variable-expanded). Empty dynamic context is represented by
    /// *not* calling this at all — an empty Developer message would read to the
    /// model as a prompt — so a call always carries real content.
    ///
    /// This is a pure tail append: it shifts no existing message, and the
    /// appended message is part of the threaded delta (it must reach the
    /// provider every turn because its content changes), so
    /// `conversation_state` needs no cursor adjustment and is deliberately not
    /// consulted here. Adjusting the delta cursor as if this were an interior
    /// insertion would push the cursor *past* the message on the turn the delta
    /// is otherwise empty, dropping it from the request.
    pub(super) fn attach(&mut self, content: String, messages: &mut Vec<Message>) {
        let idx = messages.len();
        messages.push(Message {
            response_items: Vec::new(),
            role: MessageRole::Developer,
            content: Some(content),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
        });
        self.index = Some(idx);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::r#loop::config::AgentLoopConfig;
    use crate::r#loop::conversation_state::ConversationRequestState;
    use crate::provider::tools::ProviderCapabilities;

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

    /// The message is appended at the tail, after the System prefix and all
    /// history, and its index is recorded.
    #[test]
    fn attach_places_message_at_the_tail() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "prompt"),
        ];
        let mut dev = ManagedDevMessage::new();

        dev.attach("dynamic".to_string(), &mut messages);

        assert_eq!(messages.len(), 3);
        assert_eq!(dev.index, Some(2));
        assert_eq!(messages[2].role, MessageRole::Developer);
        assert_eq!(messages[2].content.as_deref(), Some("dynamic"));
    }

    /// A full iteration boundary: attach at the tail, the loop appends real
    /// history (an assistant turn) *after* it, then the next iteration detaches
    /// the exact slot it placed — leaving the appended history intact — and
    /// re-attaches fresh at the new tail.
    #[test]
    fn detach_then_reattach_updates_the_managed_message_in_place_at_the_tail() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(1);
        let mut dev = ManagedDevMessage::new();

        // Iteration 1: attach at tail (index 2).
        dev.attach("dynamic v1".to_string(), &mut messages);
        assert_eq!(dev.index, Some(2));

        // The loop appends an assistant turn AFTER the managed message.
        messages.push(message(MessageRole::Assistant, "answer"));
        assert_eq!(messages[2].content.as_deref(), Some("dynamic v1"));

        // Iteration 2: detach removes exactly the managed slot (index 2),
        // never the assistant turn now sitting after it.
        dev.detach(&mut messages, &mut cs);
        assert_eq!(dev.index, None);
        assert_eq!(messages.len(), 3);
        let contents: Vec<&str> = messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect();
        assert_eq!(contents, vec!["system", "prompt", "answer"]);

        // Re-attach fresh content at the new tail (after the assistant turn).
        dev.attach("dynamic v2".to_string(), &mut messages);
        assert_eq!(dev.index, Some(3));
        assert_eq!(messages[3].content.as_deref(), Some("dynamic v2"));
    }

    /// Detaching when no dynamic context was ever placed is a no-op.
    #[test]
    fn detach_without_a_slot_is_a_noop() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(1);
        let mut dev = ManagedDevMessage::new();

        dev.detach(&mut messages, &mut cs);

        assert_eq!(messages.len(), 2);
        assert_eq!(dev.index, None);
    }

    /// The empty-context path: an iteration attaches nothing (the caller skips
    /// `attach` when `dynamic_context()` is `None`), so the next `detach` finds
    /// no slot and leaves history untouched — a compaction summary that now
    /// sits near the tail is never mistaken for the managed slot.
    #[test]
    fn detach_never_touches_a_history_developer_summary() {
        // History ends with a Developer-role compaction summary and no managed
        // message was attached this iteration (index None).
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "prompt"),
            message(MessageRole::Assistant, "answer"),
            message(MessageRole::Developer, "compaction summary"),
        ];
        let mut cs = state(1);
        let mut dev = ManagedDevMessage::new();

        dev.detach(&mut messages, &mut cs);

        assert_eq!(messages.len(), 4, "history summary must survive");
        assert_eq!(messages[3].content.as_deref(), Some("compaction summary"));
        assert_eq!(dev.index, None);
    }

    /// Self-heal: if the stored slot no longer holds a Developer message (a
    /// loop-invariant violation), `detach` forgets the slot without removing
    /// whatever now occupies that index.
    #[test]
    fn detach_self_heals_when_slot_is_not_developer() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(1);
        let mut dev = ManagedDevMessage::new();
        dev.attach("dynamic".to_string(), &mut messages);
        assert_eq!(dev.index, Some(2));

        // Corrupt the slot: replace the managed Developer message with a
        // non-Developer message at the same index (simulating an invariant
        // violation where something inserted/overwrote before detach).
        messages[2] = message(MessageRole::User, "not a dev message");

        dev.detach(&mut messages, &mut cs);

        assert_eq!(dev.index, None, "tracker forgets the slot");
        assert_eq!(messages.len(), 3, "nothing removed on the self-heal path");
        assert_eq!(messages[2].content.as_deref(), Some("not a dev message"));
    }

    /// The delta cursor is corrected on detach but never on attach. Removing
    /// the managed slot shifts the messages after it down, so `input_start`
    /// must follow; the subsequent tail attach must leave `input_start` alone
    /// so the fresh message stays inside the threaded delta.
    #[test]
    fn detach_shifts_the_delta_cursor_and_attach_leaves_it_alone() {
        // [System, User, Assistant(managed slot was here last turn)] with the
        // managed message at index 2 and the delta starting after the
        // assistant turn (input_start past everything real).
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "prompt"),
            message(MessageRole::Assistant, "answer"),
            message(MessageRole::Developer, "dynamic v1"),
        ];
        // Simulate a threaded state whose cursor sits past the assistant turn
        // (index 3) — i.e. the delta is empty and only the fresh managed
        // message should be sent next turn.
        let mut cs = ConversationRequestState::new(
            &AgentLoopConfig {
                conversation_state: crate::r#loop::config::ConversationStateMode::ProviderThreaded,
                ..AgentLoopConfig::default()
            },
            ProviderCapabilities::openai_responses(),
            1,
            Some(
                crate::r#loop::conversation_state::ResponseThreadAnchor::for_test(
                    "resp_prev".to_string(),
                    4,
                ),
            ),
        )
        .expect("state");
        let mut dev = ManagedDevMessage::new();
        dev.index = Some(3);

        // Detach the stale managed message at index 3; input_start (4) sits
        // after it, so it must decrement to 3.
        dev.detach(&mut messages, &mut cs);
        assert_eq!(messages.len(), 3);

        // Re-attach at the new tail (index 3). The request delta must still
        // contain exactly the fresh managed message — proving attach did not
        // push the cursor past it.
        dev.attach("dynamic v2".to_string(), &mut messages);
        let request = cs.request_messages(&messages);
        assert_eq!(
            request.last().and_then(|m| m.content.as_deref()),
            Some("dynamic v2"),
            "the fresh managed message must be last in the request delta",
        );
        assert!(
            request
                .iter()
                .filter(|m| matches!(m.role, MessageRole::Developer))
                .count()
                == 1,
            "exactly one managed Developer message in the delta",
        );
    }
}
