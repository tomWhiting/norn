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
pub mod conversion;
pub mod events;
mod jsonl_sink;
pub mod manager;
pub mod mutation_ledger;
pub mod persistence;
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
pub use manager::{CreateSessionOptions, OpenSession, ReplaySummary, SessionManager};
pub use mutation_ledger::{
    DiffStats, MutationLedger, MutationLedgerEntry, MutationOp, RecordedMutation, RevertStatus,
};
pub use persistence::{
    RESERVED_SESSION_ID_STEMS, ReplayArtifacts, SESSION_FORMAT_VERSION, SessionFileHeader,
    SessionIndexEntry, SessionPersistError, SessionStatus, append_events, append_index_entry,
    index_file_path, insert_index_entry_if_absent, is_reserved_session_id, read_index,
    read_session_events, read_session_events_for_entry, remove_index_entry,
    resolve_latest_session_in_working_dir, resolve_session, sum_usage_from_events,
    update_index_entry, update_session_index, write_index_atomic,
};
pub use resume_repair::repair_dangling_tool_calls;
pub use spool::{SpoolWriter, read_spooled_output, resolve_spool_ref};
pub use store::{DurabilityPolicy, EventStore, JsonlSink, PersistenceSink};
