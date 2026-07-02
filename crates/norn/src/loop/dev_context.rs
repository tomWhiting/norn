//! Managed dynamic-context Developer message tracking (REVIEW H2).
//!
//! The agent loop maintains at most one Developer-role message that carries
//! the current dynamic context (environment section, collaboration mode,
//! rule-injected sections, prompt-command output). Historically the loop
//! located this message by searching for the *first* Developer-role message
//! in the conversation — which, after a session resume, could be a
//! mid-history compaction summary rendered as a Developer message. The sync
//! would then overwrite or delete that summary.
//!
//! [`ManagedDevMessage`] tracks the managed slot by explicit index instead.
//! The index is only ever `Some(1)` (immediately after the System message at
//! index 0) or `None`; every other conversation mutation in the loop appends
//! at the tail or removes at indices past the prefix, so the tracked index
//! never needs rebasing.

use crate::r#loop::conversation_state::ConversationRequestState;
use crate::provider::request::{Message, MessageRole};

/// Index at which the managed Developer message is inserted: immediately
/// after the System message at index 0, ahead of all persisted history.
const MANAGED_DEV_INDEX: usize = 1;

/// Tracker for the loop-managed dynamic-context Developer message.
///
/// Constructed once per step from the initial prompt layout and consulted on
/// every iteration via [`Self::sync`]. Developer messages that arrive from
/// persisted history (compaction summaries) are never touched because the
/// tracker addresses its slot by index, not by role.
#[derive(Debug)]
pub(super) struct ManagedDevMessage {
    /// Index of the managed message in the live conversation, when present.
    ///
    /// Invariant: always `Some(MANAGED_DEV_INDEX)` or `None`. See module
    /// docs for why no rebasing is required.
    index: Option<usize>,
}

impl ManagedDevMessage {
    /// Create a tracker from the initial prompt layout.
    ///
    /// `initial_index` is `Some(1)` when `build_initial_messages` inserted a
    /// dynamic-context Developer message into the prefix, `None` otherwise.
    pub(super) const fn new(initial_index: Option<usize>) -> Self {
        Self {
            index: initial_index,
        }
    }

    /// Current index of the managed message, if one exists.
    pub(super) const fn index(&self) -> Option<usize> {
        self.index
    }

    /// Number of leading non-event prefix messages in the live conversation:
    /// the System message plus the managed Developer message when present.
    pub(super) const fn prefix_len(&self) -> usize {
        match self.index {
            Some(_) => 2,
            None => 1,
        }
    }

    /// Reconcile the managed slot with the current dynamic context.
    ///
    /// - content + existing slot: the slot's content is replaced in place;
    /// - content + no slot: a Developer message is inserted at index 1 and
    ///   `conversation_state` is told about the insertion;
    /// - no content + existing slot: the slot is removed (an empty Developer
    ///   message would be mistaken for a prompt) and `conversation_state` is
    ///   told about the removal;
    /// - no content + no slot: nothing happens.
    ///
    /// History Developer messages (compaction summaries) are never candidates
    /// because only the tracked index is read or written.
    pub(super) fn sync(
        &mut self,
        dynamic: Option<String>,
        messages: &mut Vec<Message>,
        conversation_state: &mut ConversationRequestState,
    ) {
        let slot = self.verified_slot(messages);
        match (dynamic, slot) {
            (Some(content), Some(idx)) => {
                messages[idx].content = Some(content);
            }
            (Some(content), None) => {
                messages.insert(
                    MANAGED_DEV_INDEX,
                    Message {
                        role: MessageRole::Developer,
                        content: Some(content),
                        thinking: String::new(),
                        reasoning: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tool_name: None,
                        tool_call_kind: None,
                    },
                );
                conversation_state.note_inserted_message(MANAGED_DEV_INDEX);
                self.index = Some(MANAGED_DEV_INDEX);
            }
            (None, Some(idx)) => {
                messages.remove(idx);
                conversation_state.note_removed_message(idx);
                self.index = None;
            }
            (None, None) => {}
        }
    }

    /// Return the tracked index after verifying the slot still holds a
    /// Developer message.
    ///
    /// The structural invariant (only this tracker mutates index 1; all
    /// other loop mutations are tail appends or post-prefix removals) makes
    /// a mismatch a programming error. If it ever occurs the tracker logs
    /// loudly and resets to "absent" so the next sync re-inserts a fresh
    /// managed message instead of clobbering whatever now occupies the slot.
    fn verified_slot(&mut self, messages: &[Message]) -> Option<usize> {
        let idx = self.index?;
        if messages
            .get(idx)
            .is_some_and(|m| matches!(m.role, MessageRole::Developer))
        {
            Some(idx)
        } else {
            tracing::error!(
                index = idx,
                message_count = messages.len(),
                "managed developer slot no longer holds a Developer message; \
                 resetting tracker (this indicates a loop invariant violation)",
            );
            self.index = None;
            None
        }
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

    /// A history compaction summary (Developer role, mid-conversation) must
    /// not be deleted when there is no dynamic context.
    #[test]
    fn sync_without_content_never_deletes_history_developer_message() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Developer, "compaction summary"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(1);
        let mut dev = ManagedDevMessage::new(None);

        dev.sync(None, &mut messages, &mut cs);

        assert_eq!(messages.len(), 3, "history summary must survive");
        assert_eq!(messages[1].content.as_deref(), Some("compaction summary"));
        assert_eq!(dev.index(), None);
        assert_eq!(dev.prefix_len(), 1);
    }

    /// A history compaction summary must not be overwritten when dynamic
    /// context appears mid-step; the context gets its own message at index 1.
    #[test]
    fn sync_with_content_inserts_instead_of_overwriting_history_summary() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Developer, "compaction summary"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(1);
        let mut dev = ManagedDevMessage::new(None);

        dev.sync(Some("dynamic".to_string()), &mut messages, &mut cs);

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].content.as_deref(), Some("dynamic"));
        assert_eq!(
            messages[2].content.as_deref(),
            Some("compaction summary"),
            "history summary must survive, shifted by one",
        );
        assert_eq!(dev.index(), Some(1));
        assert_eq!(dev.prefix_len(), 2);
    }

    /// An existing managed slot is updated in place.
    #[test]
    fn sync_updates_managed_slot_in_place() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Developer, "old dynamic"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(2);
        let mut dev = ManagedDevMessage::new(Some(1));

        dev.sync(Some("new dynamic".to_string()), &mut messages, &mut cs);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].content.as_deref(), Some("new dynamic"));
        assert_eq!(dev.index(), Some(1));
    }

    /// The managed slot is removed when dynamic context disappears.
    #[test]
    fn sync_removes_managed_slot_when_content_disappears() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::Developer, "dynamic"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(2);
        let mut dev = ManagedDevMessage::new(Some(1));

        dev.sync(None, &mut messages, &mut cs);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(dev.index(), None);
    }

    /// A corrupted slot (non-Developer message at the tracked index) is
    /// detected and the tracker self-heals by re-inserting.
    #[test]
    fn sync_self_heals_when_slot_is_not_developer() {
        let mut messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "prompt"),
        ];
        let mut cs = state(2);
        let mut dev = ManagedDevMessage::new(Some(1));

        dev.sync(Some("dynamic".to_string()), &mut messages, &mut cs);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, MessageRole::Developer);
        assert_eq!(messages[1].content.as_deref(), Some("dynamic"));
        assert_eq!(messages[2].role, MessageRole::User);
        assert_eq!(dev.index(), Some(1));
    }
}
