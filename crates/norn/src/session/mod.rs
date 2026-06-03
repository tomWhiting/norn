//! Session event model: append-only events, context editing, storage, tree.

pub use crate::error::SessionError;

pub mod action_log;
pub(super) mod action_log_mutations;
pub mod context_edit;
pub mod conversion;
pub mod events;
pub mod mutation_ledger;
pub mod persistence;
pub mod store;
pub mod tree;

pub use action_log::{ActionLog, ActionLogContext, ActionLogDetail, ActionLogEntry, Outcome};
pub use mutation_ledger::{
    DiffStats, MutationLedger, MutationLedgerEntry, MutationOp, RecordedMutation, RevertStatus,
};
pub use persistence::{
    SessionIndexEntry, SessionPersistError, SessionStatus, append_events, append_index_entry,
    attach_sink, create_session, fork_session, index_file_path, read_index, read_session_events,
    remove_index_entry, resolve_session, resume_session, session_file_path, sum_usage_from_events,
    update_index_entry, update_session_index, write_index_atomic,
};
pub use tree::{BranchConfig, SessionId, SessionMetadata, SessionNode, SessionTree};
