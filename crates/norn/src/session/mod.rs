//! Session event model: append-only events, context editing, storage,
//! child-session branching.

pub use crate::error::SessionError;

pub mod action_log;
pub(super) mod action_log_mutations;
pub mod action_log_scope;
pub(super) mod action_log_summary;
pub mod action_log_tree;
pub mod artifacts;
pub mod branch;
pub mod context_edit;
mod context_edit_compaction;
mod context_edit_plan;
pub mod conversion;
mod event_projection;
pub mod events;
mod jsonl_sink;
pub mod manager;
pub mod migration;
pub mod mutation_ledger;
pub mod persistence;
mod provider_affinity;
mod provider_filtered_fork_boundary;
mod provider_state_provenance;
#[cfg(test)]
mod provider_state_test_support;
mod provider_state_validation;
pub mod response_audio;
mod response_publication_commitment;
pub mod resume_repair;
pub mod spool;
pub mod store;

pub use action_log::{ActionLog, ActionLogContext, ActionLogDetail, ActionLogEntry, Outcome};
pub use action_log_scope::{ActionLogFilter, LabeledEntry, ScopedLog};
pub use action_log_tree::ActionLogTree;
pub use artifacts::SessionArtifactStore;
pub use branch::{
    BranchedChild, ChildBranchRequest, ChildDurability, ROOT_PATH_ADDRESS, SessionBinding,
    SessionBrancher, child_path_slug, slugify_name_stem,
};
pub(crate) use event_projection::{
    apply_local_tool_event, atomic_local_tool_projection, unresolved_effective_local_tool_calls,
    unresolved_local_tool_calls,
};
pub use manager::{CreateSessionOptions, OpenSession, ReplaySummary, ResumePolicy, SessionManager};
pub use migration::{
    LegacyClassificationReason, LegacySessionMigrationRecord, MigrationCounts,
    SessionMigrationManifest, SessionMigrationOutcome, export_legacy_session_raw,
    migrate_legacy_sessions, read_legacy_migration_manifest, verify_legacy_session_cutover,
    verify_legacy_session_migration,
};
pub use mutation_ledger::{
    DiffStats, MutationLedger, MutationLedgerEntry, MutationOp, RecordedMutation, RevertStatus,
};
#[cfg(test)]
pub(crate) use persistence::read_session_events;
#[cfg(test)]
pub(crate) use persistence::update_index_entry;
pub use persistence::{
    RESERVED_SESSION_ID_STEMS, ReplayArtifacts, ResumeFidelity, SESSION_FORMAT_VERSION,
    SessionFileHeader, SessionIndexEntry, SessionPersistError, SessionRecordOrigin, SessionStatus,
    is_reserved_session_id, read_index, read_session_events_for_entry,
    resolve_latest_session_in_working_dir, resolve_session, sum_usage_from_events,
};
pub use provider_filtered_fork_boundary::ProviderFilteredForkBoundary;
pub use provider_state_provenance::{
    PROVIDER_STATE_PROVENANCE_EVENT_TYPE, ProviderStateProvenance, ProviderStateProvenanceError,
};
#[cfg(test)]
pub(crate) use provider_state_test_support::{
    ResponsePublicationFixture, committed_response_publication, response_publication_fixture,
};
pub use provider_state_validation::ProviderStateValidationError;
pub(crate) use provider_state_validation::{
    ActiveResponseProvenance, ResponseStateDisposition, discover_active_response_provenance,
    event_cuts_response_anchor, response_publication_group_len, seal_response_publication_group,
    validate_new_response_publication_batches, validate_provider_state_provenance,
};
pub use response_audio::{
    RESPONSE_AUDIO_ARTIFACT_EVENT_TYPE, ResponseAudioArtifact, ResponseAudioArtifactLink,
    ResponseAudioArtifactRef, ResponseAudioArtifactState, ResponseAudioReferenceError,
    ResponseAudioStore, referenced_response_audio_artifacts, response_audio_artifact_links,
};
pub use response_publication_commitment::ResponsePublicationCommitment;
pub(crate) use resume_repair::is_interrupted_tool_result;
pub use resume_repair::repair_dangling_tool_calls;
pub use spool::{SpoolWriter, read_spooled_output, resolve_spool_ref};
pub use store::{DurabilityPolicy, EventStore, JsonlSink, PersistenceSink};

#[cfg(test)]
mod canonical_persistence_tests;
#[cfg(test)]
mod canonical_tool_resolution_tests;
#[cfg(test)]
mod canonical_transcript_tests;
#[cfg(test)]
mod provider_affinity_embedder_tests;
#[cfg(test)]
mod provider_affinity_stale_tests;
#[cfg(test)]
mod provider_affinity_tests;
#[cfg(test)]
mod provider_epoch_tests;
#[cfg(test)]
mod response_audio_lifecycle_tests;
#[cfg(test)]
mod response_publication_batch_tests;
#[cfg(test)]
mod response_publication_commitment_tests;
