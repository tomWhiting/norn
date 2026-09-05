//! Source-bound cursor, item and execution vocabulary; no terminal coordinates.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::body::{BodyRef, DisplayText};
use super::error::ViewError;
use super::tools::ToolView;
use crate::model_selection::{CatalogBackend, ModelRuntime};
use crate::provider::request::{ReasoningEffort, ServiceTier};
use crate::session::events::EventId;

/// A persisted session identity or an explicitly process-local identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SessionIdentity {
    /// Actual persisted session identifier supplied by its owner.
    Persisted(String),
    /// Explicit ephemeral identity; not a path to a persisted session.
    Ephemeral(Uuid),
}

/// One agent timeline in one actual local store instance.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewSource {
    /// Session identity, never inferred from the working directory.
    pub session: SessionIdentity,
    /// Actual emitting or receiving runtime agent.
    pub agent_id: Uuid,
    /// Parent runtime agent when this is a child timeline.
    pub parent_agent_id: Option<Uuid>,
    /// Generation minted by the store owner for this local store instance.
    /// Reopen/replacement changes it; resize does not. It is not durable.
    pub store_generation: Uuid,
}

/// A position in append order, distinct from provider-context order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HistoryPosition {
    /// Before the first record, including a truly empty store.
    Empty,
    /// Exact accepted event and its zero-based append ordinal.
    Event {
        /// Validated append-order position.
        ordinal: usize,
        /// Event occupying that position.
        event_id: EventId,
    },
}

/// Direction through the projection's committed-then-live display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemDirection {
    /// Visit the nearest preceding row first, moving toward the beginning.
    Earlier,
    /// Visit following rows in forward display order.
    Later,
}

/// Whether traversal includes its exact current anchor item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemInclusion {
    /// Yield the anchor before its neighbors.
    Inclusive,
    /// Begin with the neighbor in the requested direction.
    Exclusive,
}

/// History position bound to session, agent and local store generation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HistoryCursor {
    /// Source against which every component must be validated.
    pub(crate) source: ViewSource,
    /// Empty-start or exact record position.
    pub(crate) position: HistoryPosition,
}

impl HistoryCursor {
    /// Build a distinct empty-start cursor for a named source.
    #[must_use]
    pub(crate) fn empty(source: ViewSource) -> Self {
        Self {
            source,
            position: HistoryPosition::Empty,
        }
    }

    /// Bind an event to the append ordinal validated by its store owner.
    #[must_use]
    pub(crate) fn event(source: ViewSource, ordinal: usize, event_id: EventId) -> Self {
        Self {
            source,
            position: HistoryPosition::Event { ordinal, event_id },
        }
    }

    /// Validate this cursor against a store-owner supplied record.
    pub(crate) fn validate(
        &self,
        source: &ViewSource,
        ordinal: usize,
        event_id: &EventId,
    ) -> Result<(), ViewError> {
        if &self.source != source {
            return Err(ViewError::SourceMismatch {
                expected: Box::new(source.clone()),
                actual: Box::new(self.source.clone()),
            });
        }
        match &self.position {
            HistoryPosition::Event {
                ordinal: actual,
                event_id: actual_id,
            } if *actual == ordinal && actual_id == event_id => Ok(()),
            HistoryPosition::Empty | HistoryPosition::Event { .. } => {
                Err(ViewError::CursorMismatch {
                    event_id: event_id.clone(),
                })
            }
        }
    }

    /// Read the source bound by the owning store.
    #[must_use]
    pub const fn source(&self) -> &ViewSource {
        &self.source
    }

    /// Inspect the position without minting another capability.
    #[must_use]
    pub const fn position(&self) -> &HistoryPosition {
        &self.position
    }
}

/// Caller-owned local execution and the response attempt inside it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AttemptKey {
    /// Local admission identity, never a provider response identifier.
    pub execution: Uuid,
    /// Zero-based response iteration inside that admitted execution.
    pub response: u64,
    /// Actual provider attempt number; first attempt is one.
    pub attempt: u32,
}

/// Semantic stream segment, preserving supplied identities where available.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SegmentKey {
    /// Ordered local text segment, without a claimed provider identity.
    Text(u64),
    /// Ordered local display-summary segment.
    Thinking(u64),
    /// Identity-bearing refusal content part.
    Refusal {
        /// Actual provider item identifier.
        item_id: String,
        /// Actual output index.
        output_index: u64,
        /// Actual content index.
        content_index: u64,
    },
    /// A completed provider item and its display-only field ordinal.
    ResponsePart {
        /// Actual provider output item identifier, if supplied.
        item_id: Option<String>,
        /// Actual output index, if supplied.
        output_index: Option<u64>,
        /// Local ordered display part inside that item.
        part: usize,
    },
    /// Streaming tool item identifier; never substituted for a call ID.
    ToolItem(String),
    /// Actual provider call identifier.
    ToolCall(String),
    /// Result body for the actual call identifier.
    ToolResult(String),
    /// Display-only local notice with no fabricated provider identity.
    Notice(u64),
}

/// Volatile identity for one source/attempt/semantic segment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProvisionalKey {
    /// Exact owning source and agent.
    pub source: ViewSource,
    /// Explicit local attempt association.
    pub attempt: AttemptKey,
    /// Semantic segment inside that attempt.
    pub segment: SegmentKey,
}

/// Stable semantic item identity, independent of terminal rows and wrapping.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemId {
    /// Accepted store event and its ordered display part.
    Committed {
        /// Validated owning event.
        cursor: HistoryCursor,
        /// Display part inside that event.
        part: usize,
    },
    /// Volatile stream fragment identity.
    Provisional(ProvisionalKey),
    /// Frontend-owned notice outside a provider execution.
    Local {
        /// Owning source.
        source: ViewSource,
        /// Monotonic local notice ordinal.
        ordinal: u64,
    },
}

/// Actual accepted model configuration captured at local turn admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedModel {
    /// Canonical accepted model ID.
    pub model: DisplayText,
    /// Actual catalogue route, absent when the provider has none.
    pub backend: Option<CatalogBackend>,
    /// Effective accepted context window.
    pub context_window: u64,
    /// Actual selected reasoning effort, when explicit.
    pub effort: Option<ReasoningEffort>,
    /// Actual selected service tier, when explicit.
    pub tier: Option<ServiceTier>,
    /// Caller-owned configuration revision at admission.
    pub configuration_revision: u64,
}

impl AcceptedModel {
    /// Capture accepted runtime policy without applying or changing it.
    #[must_use]
    pub fn capture(runtime: &ModelRuntime, configuration_revision: u64) -> Self {
        Self {
            model: DisplayText::new(runtime.model()),
            backend: runtime.backend(),
            context_window: runtime.window(),
            effort: runtime.effort(),
            tier: runtime.tier(),
            configuration_revision,
        }
    }
}

/// Semantic category; it does not contain opaque provider replay state.
#[derive(Clone, Debug)]
pub enum ViewItemKind {
    /// Stored input with unknown legacy origin unless explicitly associated.
    Input,
    /// Provider answer text.
    Text,
    /// Approved human-readable display reasoning summary.
    Thinking,
    /// Model refusal rather than a transport failure.
    Refusal,
    /// Exact tool invocation/result evidence.
    Tool(Box<ToolView>),
    /// Validated structured spoken or child result output.
    Structured,
    /// Historical model transition, never applied by the view.
    ModelChange {
        /// Stored old model label.
        old: DisplayText,
        /// Stored new model label.
        new: DisplayText,
    },
    /// Child/session structural metadata.
    Child,
    /// Source-attributed external input; cannot execute UI commands.
    ExternalInput,
    /// Context compaction or bookkeeping, not replacement visible history.
    Context,
    /// Explicit error or refusal of a view/runtime operation.
    Error,
    /// Label, retry, lifecycle or observability-only metadata.
    Notice,
    /// Routine runtime accounting/provenance retained for explicit details,
    /// outside the default conversation. This never denotes an error or refusal.
    Metadata,
    /// Payload has no approved display capability.
    Unavailable,
}

/// One semantic row with lazy bodies and explicit source/coverage evidence.
#[derive(Clone, Debug)]
pub struct ViewItem {
    /// Stable item identity or explicitly provisional identity.
    pub id: ItemId,
    /// Semantic interpretation.
    pub kind: ViewItemKind,
    /// Compact, terminal-safe label; never an invented tool description.
    pub label: DisplayText,
    /// Typed body capabilities, loaded only on explicit demand.
    pub bodies: Vec<BodyRef>,
    /// Accepted local model when proven; historical unknown remains absent.
    pub model: Option<AcceptedModel>,
}

/// Store-owner-minted compact projection of one accepted event. It contains no
/// raw event, provider transport state, encrypted reasoning or spool bytes.
#[derive(Clone, Debug)]
pub struct HistoryRecord {
    pub(crate) cursor: HistoryCursor,
    pub(crate) items: Vec<ViewItem>,
    pub(crate) assistant: bool,
    pub(crate) parts: Vec<CommittedPartIdentity>,
}

impl HistoryRecord {
    /// Accepted position validated by the store owner.
    #[must_use]
    pub const fn cursor(&self) -> &HistoryCursor {
        &self.cursor
    }
    /// Compact display metadata and lazy body capabilities.
    #[must_use]
    pub fn items(&self) -> &[ViewItem] {
        &self.items
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CommittedPartIdentity {
    pub row: usize,
    pub item_id: String,
    pub part: usize,
}

/// A missing or inexact part of live coverage, retained after history catches up.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CoverageGap {
    /// Broadcast lag means transient states/messages may be absent.
    BroadcastLag,
    /// Generic events cannot prove exact provisional-to-committed identity.
    IncompleteAssociation,
    /// A committed page did not include all preceding events.
    OlderHistoryMissing,
    /// An execution ended before all provisional work reached a terminal result.
    Interrupted,
}

/// Observable coverage; store acceptance is not an fsync or Ready guarantee.
#[derive(Clone, Debug)]
pub struct ViewCoverage {
    /// Explicit incompleteness that history reconciliation cannot invent away.
    pub gaps: BTreeSet<CoverageGap>,
    /// Number of live events reported missing by the broadcast owner.
    pub missed_live_events: u64,
    /// Highest observed committed cursor; not a gap-free feed watermark.
    pub observed_cursor: HistoryCursor,
}
