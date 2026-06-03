//! Re-exports for JSONL-backed session persistence.
//!
//! The implementation lives in `norn::session` so CLI, TUI, and workflow
//! consumers all use the same session index and JSONL storage.

pub use norn::session::{
    SessionIndexEntry, SessionPersistError, SessionStatus, append_events, append_index_entry,
    attach_sink, create_session, fork_session, index_file_path, read_index, read_session_events,
    remove_index_entry, resolve_session, resume_session, session_file_path, sum_usage_from_events,
    update_index_entry, update_session_index, write_index_atomic,
};
