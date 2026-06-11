//! JSONL-backed session persistence shared by norn consumers.
//!
//! Layout under `paths::session_data_dir()` (NC-002 R1):
//!
//! ```text
//! sessions/
//! +-- index.jsonl          one [`SessionIndexEntry`] per line
//! +-- index.lock           advisory inter-process lock guarding every
//!                           index mutation (created on first use)
//! +-- {session_id}.jsonl   version-header line, then one
//!                           [`SessionEvent`](crate::session::events::SessionEvent)
//!                           per line, append-only
//! +-- ...
//! +-- index.jsonl.tmp.*    transient -- present only during an atomic
//!                           index rewrite that has not yet been renamed
//! ```

pub mod index;
pub mod io;
mod lock;
pub mod ops;
pub mod types;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

pub use index::{
    append_index_entry, index_file_path, read_index, remove_index_entry, resolve_session,
    sum_usage_from_events, update_index_entry, update_session_index, write_index_atomic,
};
pub use io::{append_events, read_session_events, session_file_path};
pub use ops::{attach_sink, create_session, fork_session, resume_session};
pub use types::{
    SESSION_FORMAT_VERSION, SessionFileHeader, SessionFileRead, SessionIndexEntry,
    SessionPersistError, SessionStatus,
};
