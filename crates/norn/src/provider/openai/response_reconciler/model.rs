//! Public state and error types for response reconciliation.

use std::cmp::Ordering;

use thiserror::Error;

use crate::provider::response_item::ResponseTranscriptItem;

/// Stable identity of one item within a response output.
///
/// The output position is always present and is the map key. A provider item
/// identifier is an optional assertion about that position: when supplied, the
/// reconciler separately enforces its one-to-one binding in both directions.
#[derive(Clone, Debug)]
pub struct ResponseItemIdentity {
    pub(super) item_id: Option<String>,
    pub(super) output_index: u64,
}

impl ResponseItemIdentity {
    /// Provider item identifier, when this item family supplied one.
    #[must_use]
    pub fn item_id(&self) -> Option<&str> {
        self.item_id.as_deref()
    }

    /// Position in the terminal response output.
    #[must_use]
    pub const fn output_index(&self) -> u64 {
        self.output_index
    }
}

impl PartialEq for ResponseItemIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.output_index == other.output_index
    }
}

impl Eq for ResponseItemIdentity {}

impl PartialOrd for ResponseItemIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ResponseItemIdentity {
    fn cmp(&self, other: &Self) -> Ordering {
        self.output_index.cmp(&other.output_index)
    }
}

/// Identity-keyed channel for one kind of streamed delta.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ResponseDeltaChannel {
    /// Assistant output text at a message content index.
    OutputText(u64),
    /// Assistant refusal text at a message content index.
    Refusal(u64),
    /// Structured function-call arguments.
    FunctionCallArguments,
    /// Freeform custom-tool input.
    CustomToolCallInput,
    /// Reasoning summary text at a summary index.
    ReasoningSummaryText(u64),
    /// Detailed reasoning text at a content index.
    ReasoningText(u64),
}

/// How one preview channel compared with authoritative completed content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeltaReconciliationDisposition {
    /// The accumulated preview already matched completion data exactly.
    Matched,
    /// Completion data replaced a conflicting or truncated preview.
    Repaired,
    /// No preview existed, so completion data populated the channel.
    Synthesized,
}

/// Result of reconciling one delta channel with authoritative content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeltaReconciliation {
    /// Identity-keyed channel that was reconciled.
    pub channel: ResponseDeltaChannel,
    /// Whether completion matched, repaired, or synthesized preview state.
    pub disposition: DeltaReconciliationDisposition,
}

/// Result of accepting one stream frame.
#[derive(Clone, Debug, PartialEq)]
pub enum ReconcileUpdate {
    /// A relevant frame changed reconciliation state.
    Accepted,
    /// A known or future frame required no reconciliation state.
    Ignored,
    /// An identical frame with the same sequence number was already applied.
    DuplicateSequence {
        /// Sequence number of the already-applied frame.
        sequence_number: u64,
    },
    /// An exact repeated completed item was already retained.
    DuplicateCompletion {
        /// Stable identity of the already-retained item.
        identity: ResponseItemIdentity,
    },
    /// An exact repeated channel-completion event was already applied.
    DuplicateChannelCompletion,
    /// An authoritative item was retained and reconciled with its previews.
    CompletedItem {
        /// Canonical completed item with stream provenance.
        item: Box<ResponseTranscriptItem>,
        /// Per-channel preview reconciliation outcomes.
        delta_reconciliations: Vec<DeltaReconciliation>,
    },
    /// A terminal frame produced the authoritative ordered transcript.
    Terminal {
        /// Canonical items in terminal `response.output` order.
        items: Vec<ResponseTranscriptItem>,
        /// Reconciliation outcomes for items synthesized at termination.
        delta_reconciliations: Vec<DeltaReconciliation>,
    },
}

/// Structural failure while reconciling a Responses stream.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum ResponseReconciliationError {
    /// Every public Responses event carries a sequence number.
    #[error("Responses stream event had no sequence_number")]
    MissingSequenceNumber,
    /// Sequence numbers are unsigned integers.
    #[error("Responses stream event carried an invalid sequence_number")]
    InvalidSequenceNumber,
    /// A sequence number was reused for different content.
    #[error("Responses stream sequence {sequence_number} had conflicting frames")]
    ConflictingDuplicateSequence {
        /// Reused provider sequence number.
        sequence_number: u64,
    },
    /// A previously unseen frame arrived behind the accepted high-water mark.
    #[error("Responses stream sequence {sequence_number} followed {highest_sequence_number}")]
    NonMonotonicSequence {
        /// Newly received sequence number.
        sequence_number: u64,
        /// Greatest sequence number already accepted.
        highest_sequence_number: u64,
    },
    /// No frame is accepted after a terminal result.
    #[error("Responses stream produced a frame after its terminal event")]
    PostTerminalFrame,
    /// A prior protocol error permanently poisoned this reconciler.
    #[error("Responses stream reconciler was already failed")]
    AlreadyFailed,
    /// A pinned public event had no reconciliation-role entry.
    #[error("pinned Responses event had no reconciliation role")]
    UnclassifiedPublicEvent,
    /// Response-scoped audio cannot be persisted by the current transcript.
    #[error("Responses stream media is not yet supported end to end")]
    UnsupportedResponseMedia,
    /// A required envelope field was absent or had the wrong type.
    #[error("{event_type} missing or invalid {field}")]
    InvalidEnvelopeField {
        /// Pinned event category being decoded.
        event_type: &'static str,
        /// Required field path.
        field: &'static str,
    },
    /// One item identifier was rebound to another output position.
    #[error("response item identity moved between output positions")]
    ItemIdRebound {
        /// Provider item identifier that was reused.
        item_id: String,
        /// First accepted output position.
        prior_index: u64,
        /// Conflicting output position.
        new_index: u64,
    },
    /// One output position was rebound to another item identifier.
    #[error("response output {output_index} changed item identity")]
    OutputIndexRebound {
        /// Output position carrying conflicting item identifiers.
        output_index: u64,
    },
    /// A delta did not refer to an announced item identity.
    #[error("response delta referred to an unannounced item")]
    UnannouncedDeltaIdentity,
    /// A channel completion did not refer to a prior item announcement.
    #[error("response channel completion referred to an unannounced item")]
    UnannouncedChannelCompletionIdentity,
    /// A channel completion did not agree with its announced item family.
    #[error("response channel completion changed its announced item family")]
    ChannelCompletionItemKindConflict,
    /// A delta followed authoritative completion of that same channel.
    #[error("response delta arrived after channel completion")]
    DeltaAfterChannelCompletion,
    /// A delta arrived after its item had authoritative completion.
    #[error("response delta arrived after output item completion")]
    DeltaAfterCompletion,
    /// A known item was structurally invalid.
    #[error("{event_type} carried a malformed response item: {reason}")]
    MalformedItem {
        /// Pinned event category carrying the item.
        event_type: &'static str,
        /// Non-provider-controlled structural parse failure.
        reason: String,
    },
    /// The stream envelope and embedded item disagreed about identity.
    #[error("response item envelope disagreed with the embedded item id")]
    EmbeddedItemIdConflict,
    /// Repeated authoritative completion data was not byte-equivalent JSON.
    #[error("response item completion conflicted with an earlier completion")]
    ConflictingCompletion,
    /// Repeated authoritative channel completion changed its final value.
    #[error("response channel completion conflicted with an earlier completion")]
    ConflictingChannelCompletion,
    /// A new channel completion followed authoritative item completion.
    #[error("response channel completion arrived after output item completion")]
    ChannelCompletionAfterItemCompletion,
    /// Repeated `output_item.added` data changed for one stable identity.
    #[error("response item announcement conflicted with an earlier announcement")]
    ConflictingAddedItem,
    /// One provider call identifier was rebound to another output item.
    #[error("response call identity was reused by another output item")]
    CallIdReused,
    /// An authoritative call changed its announced correlation identifier.
    #[error("authoritative response call changed its announced call id")]
    AnnouncedCallIdConflict,
    /// An authoritative call changed its announced tool name.
    #[error("authoritative response call changed its announced name")]
    AnnouncedCallNameConflict,
    /// An authoritative item changed the family announced for its identity.
    #[error("authoritative response item changed its announced item family")]
    AddedItemKindConflict,
    /// Delta and completed-item families did not agree.
    #[error("response item completion did not match its accumulated delta family")]
    DeltaItemKindConflict,
    /// Item-level authority disagreed with an earlier channel completion.
    #[error("response item completion conflicted with completed channel content")]
    ChannelItemCompletionConflict,
    /// A completed content part needed by a delta was structurally invalid.
    #[error("authoritative response content was malformed: {reason}")]
    MalformedAuthoritativeContent {
        /// Non-provider-controlled structural reason.
        reason: &'static str,
    },
    /// The terminal response omitted its ordered output array.
    #[error("terminal Responses event had no response.output array")]
    MissingTerminalOutput,
    /// This platform could not represent a terminal output position as `u64`.
    #[error("terminal response output index was not representable: {reason}")]
    OutputIndexOverflow {
        /// Integer conversion failure.
        reason: String,
    },
    /// This platform could not represent a content position as `u64`.
    #[error("response content index was not representable: {reason}")]
    ContentIndexOverflow {
        /// Integer conversion failure.
        reason: String,
    },
    /// A completed item did not appear exactly in terminal output.
    #[error("completed response item conflicted with terminal response.output")]
    TerminalCompletionConflict,
    /// A completed item was absent from terminal output.
    #[error("completed response item was absent from terminal response.output")]
    CompletionAbsentFromTerminal,
    /// A channel-completed item was absent from terminal output.
    #[error("channel-completed response item was absent from terminal response.output")]
    ChannelCompletionAbsentFromTerminal,
    /// An item-scoped event referred to an identity not yet announced.
    #[error("item-scoped response event referred to an unannounced item")]
    UnannouncedItemScopedIdentity,
    /// An item-scoped event disagreed with its announced item family.
    #[error("item-scoped response event changed its announced item family")]
    ItemScopedFamilyConflict,
    /// Item-scoped preview data was repeated with conflicting content.
    #[error("item-scoped response preview conflicted with an earlier preview")]
    ConflictingItemScopedPreview,
    /// Item-scoped completion was repeated with conflicting content.
    #[error("item-scoped response completion conflicted with an earlier completion")]
    ConflictingItemScopedCompletion,
    /// Item-scoped data arrived after its channel or item had completed.
    #[error("item-scoped response data arrived after completion")]
    ItemScopedEventAfterCompletion,
    /// An annotation arrived before its containing content part.
    #[error("response annotation arrived without its content part")]
    AnnotationWithoutContentPart,
    /// A hosted operation emitted an invalid lifecycle transition.
    #[error("hosted response operation emitted a conflicting lifecycle transition")]
    ConflictingHostedLifecycle,
    /// Completed item content disagreed with prior item-scoped completion.
    #[error("completed response item conflicted with item-scoped stream authority")]
    ItemScopedCompletionConflict,
    /// A hosted completed item omitted content required by its public schema.
    #[error("completed {item_type} omitted required {field}")]
    MissingAuthoritativeItemField {
        /// Pinned, non-provider-controlled item family.
        item_type: &'static str,
        /// Pinned, non-provider-controlled field path.
        field: &'static str,
    },
    /// Item-scoped state had no corresponding item in terminal output.
    #[error("item-scoped response state was absent from terminal output")]
    ItemScopedStateAbsentFromTerminal,
    /// The provider emitted an item type outside the pinned public contract.
    #[error("Responses stream carried an unknown output item type")]
    UnknownOutputItemType {
        /// Canonical authoritative items retained before the stream failed closed.
        retained_items: Vec<ResponseTranscriptItem>,
    },
    /// The provider requested a pinned executable item Norn cannot execute.
    #[error("Responses stream requested an unsupported executable output item")]
    UnsupportedExecutableItem {
        /// Canonical authoritative items retained before the stream failed closed.
        retained_items: Vec<ResponseTranscriptItem>,
    },
    /// An executable item never gained completed authoritative state.
    #[error("executable response item remained unresolved at stream termination")]
    UnresolvedActionableItem {
        /// Canonical authoritative items retained before the stream failed closed.
        retained_items: Vec<ResponseTranscriptItem>,
    },
    /// An executable delta never gained an authoritative completed item.
    #[error("actionable call existed only as streamed deltas")]
    DeltaOnlyActionableCall,
}

impl ResponseReconciliationError {
    /// Return canonical items retained by a capability or resolution failure.
    ///
    /// The returned items are deliberately excluded from [`std::fmt::Display`]
    /// so ordinary error rendering cannot disclose provider-controlled content.
    #[must_use]
    pub fn retained_items(&self) -> &[ResponseTranscriptItem] {
        match self {
            Self::UnknownOutputItemType { retained_items }
            | Self::UnsupportedExecutableItem { retained_items }
            | Self::UnresolvedActionableItem { retained_items } => retained_items,
            _ => &[],
        }
    }
}
