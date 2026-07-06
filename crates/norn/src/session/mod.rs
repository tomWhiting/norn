//! Session event model: append-only events, context editing, storage, tree.

pub use crate::error::SessionError;

pub mod action_log;
pub(super) mod action_log_mutations;
pub mod action_log_scope;
pub(super) mod action_log_summary;
pub mod action_log_tree;
pub mod context_edit;
pub mod conversion;
pub mod events;
pub mod manager;
pub mod mutation_ledger;
pub mod persistence;
pub mod spool;
pub mod store;
pub mod tree;

pub use action_log::{ActionLog, ActionLogContext, ActionLogDetail, ActionLogEntry, Outcome};
pub use action_log_scope::{ActionLogFilter, LabeledEntry, ScopedLog};
pub use action_log_tree::ActionLogTree;
pub use manager::{CreateSessionOptions, OpenSession, ReplaySummary, SessionManager};
pub use mutation_ledger::{
    DiffStats, MutationLedger, MutationLedgerEntry, MutationOp, RecordedMutation, RevertStatus,
};
pub use persistence::{
    RESERVED_SESSION_ID_STEMS, ReplayArtifacts, SESSION_FORMAT_VERSION, SessionFileHeader,
    SessionIndexEntry, SessionPersistError, SessionStatus, append_events, append_index_entry,
    index_file_path, insert_index_entry_if_absent, is_reserved_session_id, read_index,
    read_session_events, remove_index_entry, resolve_latest_session_in_working_dir,
    resolve_session, session_file_path, sum_usage_from_events, update_index_entry,
    update_session_index, write_index_atomic,
};
pub use spool::{SpoolWriter, read_spooled_output, resolve_spool_ref};
pub use store::{DurabilityPolicy, EventStore, JsonlSink, PersistenceSink};
pub use tree::{BranchConfig, SessionId, SessionMetadata, SessionNode, SessionTree};
