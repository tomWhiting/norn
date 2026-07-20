//! Strict session-index row types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::provider::ProviderStateIdentity;

/// Lifecycle status recorded in the session index. Serialised as
/// lowercase strings (`"active"` / `"completed"`) per NC-002 R3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// Session is live and may still accept new events.
    Active,
    /// Session has been finalised and will receive no further events.
    Completed,
}

/// How faithfully a persisted timeline can participate in future execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeFidelity {
    /// The canonical persisted representation is complete and replayable.
    Canonical,
    /// Visible history is usable only after starting a fresh provider-state epoch.
    FreshEpochProjection,
    /// The timeline is retained for inspection and export, not execution.
    InspectOnly,
}

impl ResumeFidelity {
    /// Whether this classification permits an execution resume.
    #[must_use]
    pub const fn permits_resume(self) -> bool {
        !matches!(self, Self::InspectOnly)
    }

    /// Whether resume must discard any legacy provider-state anchor.
    #[must_use]
    pub const fn requires_fresh_epoch(self) -> bool {
        matches!(self, Self::FreshEpochProjection)
    }
}

/// Visible-history lineage of one active session-store record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionRecordOrigin {
    /// The visible history has no migrated-legacy lineage.
    Native,
    /// The visible history descends from a one-shot legacy migration.
    ///
    /// Forked descendants preserve this lineage: the digest identifies the
    /// legacy root history incorporated into the descendant, not merely the
    /// process that wrote the current row.
    MigratedLegacy {
        /// Source session format observed by the migrator.
        source_format: u32,
        /// Lowercase SHA-256 digest of the exact legacy source bytes.
        source_sha256: String,
    },
}

/// One row in `index.jsonl`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIndexEntry {
    /// Session identifier — a UUID v4 by default (R8: v7's shared
    /// timestamp prefix defeats git-style short-prefix eyeballing), or a
    /// validated caller-supplied opaque string.
    pub id: String,
    /// Immutable random identity for this exact incarnation of `id`.
    ///
    /// Deleting and recreating the same session id mints a new generation so
    /// stale sinks and deferred mutations cannot attach to the replacement.
    pub generation: Uuid,
    /// Optional human-readable name (set via `/name` or `--session-name`).
    pub name: Option<String>,
    /// Model identifier active when the session was created.
    pub model: String,
    /// Working directory the session was started in.
    pub working_dir: String,
    /// Creation timestamp (ISO 8601 / RFC 3339 with `chrono` `serde` feature).
    pub created_at: DateTime<Utc>,
    /// Most recent append timestamp.
    pub updated_at: DateTime<Utc>,
    /// Total number of events written to the session JSONL file.
    pub event_count: u64,
    /// Lifecycle status.
    pub status: SessionStatus,
    /// Session JSONL schema version of the writer that created the
    /// session. Active rows must equal
    /// [`SESSION_FORMAT_VERSION`](super::types::SESSION_FORMAT_VERSION).
    pub format_version: u32,
    /// Cumulative input tokens across all turns.
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all turns.
    pub total_output_tokens: u64,
    /// Cumulative cache-read tokens across all turns.
    pub total_cache_read_tokens: u64,
    /// The session file's path **relative to the data directory**. Child
    /// sessions live at `{root-id}/children/{path-slug}.jsonl`; active root
    /// sessions use `None` and resolve to `{id}.jsonl`. Legacy paths are
    /// classified and rewritten by the explicit migration before any row
    /// enters this active index. Discovery stays index-driven: the runtime
    /// never crawls the directory to infer sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rel_path: Option<String>,
    /// Session id of the parent this session was branched from (the
    /// index-side half of the durable parent↔child linkage; the
    /// event-side half is the parent's `ChildBranch` event). `None` for
    /// root sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Fidelity assigned during migration or native format-2 creation.
    pub fidelity: ResumeFidelity,
    /// Visible-history lineage retained across descendant forks.
    pub origin: SessionRecordOrigin,
    /// Opaque credential-and-authority identity that owns provider state.
    ///
    /// Absence means no provider identity has been bound yet. A managed
    /// session may adopt one identity exactly once; persistence never clears
    /// or replaces it implicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_state_identity: Option<ProviderStateIdentity>,
}

impl SessionIndexEntry {
    /// Whether execution may resume from this stored timeline.
    #[must_use]
    pub const fn permits_resume(&self) -> bool {
        self.fidelity.permits_resume()
    }

    /// Whether resume must begin a fresh provider-state epoch.
    ///
    /// Every migrated legacy record returns `true`, including records whose
    /// visible event representation is otherwise canonical. Legacy provider
    /// anchors are never promoted into the active format-2 store.
    #[must_use]
    pub const fn requires_fresh_provider_epoch(&self) -> bool {
        matches!(self.origin, SessionRecordOrigin::MigratedLegacy { .. })
            || self.fidelity.requires_fresh_epoch()
    }
}
