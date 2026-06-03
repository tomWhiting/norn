//! JSONL-backed session persistence shared by norn consumers.
//!
//! Layout under `paths::session_data_dir()` (NC-002 R1):
//!
//! ```text
//! sessions/
//! +-- index.jsonl          one [`SessionIndexEntry`] per line
//! +-- {session_id}.jsonl   one [`SessionEvent`](crate::session::events::SessionEvent) per line, append-only
//! +-- ...
//! +-- index.jsonl.tmp      transient -- present only during an atomic
//!                           index rewrite that has not yet been renamed
//! ```

pub mod io;
pub mod ops;
pub mod types;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

pub use io::{
    append_events, append_index_entry, index_file_path, index_tmp_path, read_index,
    read_session_events, remove_index_entry, resolve_session, session_file_path,
    sum_usage_from_events, update_index_entry, update_session_index, write_index_atomic,
};
pub use ops::{attach_sink, create_session, fork_session, resume_session};
pub use types::{SessionIndexEntry, SessionPersistError, SessionStatus};
