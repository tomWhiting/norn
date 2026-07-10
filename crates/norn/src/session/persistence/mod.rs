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
//! +-- {root_id}/           a persisted root's child-session directory
//!     +-- children/         (see [`crate::session::branch`]): one
//!         +-- {slug}.jsonl   full-path-slug-keyed timeline per child,
//!                            located through the index row's
//!                            [`SessionIndexEntry::rel_path`] — never a
//!                            directory crawl
//! +-- ...
//! +-- index.jsonl.tmp.*    transient -- present only during an atomic
//!                           index rewrite that has not yet been renamed
//! ```
//!
//! Session IDs share the data directory with the persistence layer's own
//! files, so the `index` name family is reserved and rejected as a
//! session ID at every boundary — see
//! [`io::RESERVED_SESSION_ID_STEMS`] / [`io::is_reserved_session_id`].

pub mod index;
pub mod io;
mod lock;
pub(crate) mod permissions;
pub mod replay;
pub mod types;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

pub use index::{
    append_index_entry, index_file_path, insert_index_entry_if_absent, read_index,
    remove_index_entry, resolve_latest_session_in_working_dir, resolve_session,
    sum_usage_from_events, update_index_entry, update_session_index, write_index_atomic,
};
pub use io::{
    RESERVED_SESSION_ID_STEMS, append_events, is_reserved_session_id, read_session_events,
    read_session_events_for_entry, resolved_session_file_path, session_file_path,
};
pub use replay::ReplayArtifacts;
pub use types::{
    SESSION_FORMAT_VERSION, SessionFileHeader, SessionIndexEntry, SessionPersistError,
    SessionStatus,
};
