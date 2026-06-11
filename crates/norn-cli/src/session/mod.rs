//! Re-exports for JSONL-backed session persistence.
//!
//! The implementation lives in `norn::session` so CLI, TUI, and workflow
//! consumers all use the same session index and JSONL storage. The
//! lifecycle (create / resume / fork / open-or-resume / list / delete)
//! goes through [`SessionManager`]; the remaining re-exports are the
//! engine primitives the CLI's command surface and tests still touch
//! directly.

pub use norn::session::{
    CreateSessionOptions, OpenSession, SessionIndexEntry, SessionManager, SessionPersistError,
    SessionStatus, append_events, append_index_entry, read_index, read_session_events,
    session_file_path, write_index_atomic,
};
