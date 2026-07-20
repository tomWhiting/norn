//! JSONL-backed session persistence shared by norn consumers.
//!
//! Layout under the standard directory returned by
//! [`crate::config::paths::resolve_standard_session_data_dir`], or under the
//! embedder-owned directory passed to [`crate::session::SessionManager::new`]:
//!
//! ```text
//! session-store/
//! +-- index.jsonl          exact format header, then one
//!                           [`SessionIndexEntry`] per line
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
//! +-- index.jsonl.tmp.<uuid>
//!                         exact writer-owned temporary for an interrupted
//!                         atomic index rewrite; reclaimed under `index.lock`
//! ```
//!
//! Session IDs share the data directory with the persistence layer's own
//! files, so the `index` name family is reserved and rejected as a
//! session ID at every boundary — see
//! [`io::RESERVED_SESSION_ID_STEMS`] / [`io::is_reserved_session_id`].

mod admission;
mod counters;
mod event_reader;
pub mod index;
mod index_entry;
pub mod io;
mod lock;
pub mod replay;
pub mod strict;
mod strict_runtime;
mod timeline_file;
mod timeline_lock;
pub mod types;

#[cfg(test)]
mod counter_overflow_tests;
#[cfg(test)]
mod deletion_runtime_tests;
#[cfg(test)]
mod index_deadline_tests;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
#[cfg(test)]
mod timeline_concurrency_tests;
#[cfg(test)]
mod timeline_runtime_tests;

pub(crate) use admission::acquire_private_fs;
pub(crate) use counters::IndexCounters;
pub(crate) use event_reader::read_session_events_for_entry_with_deadline;
#[cfg(test)]
pub(crate) use index::{
    append_index_entry, insert_index_entry_if_absent, update_index_entry, update_session_index,
};
#[cfg(test)]
pub(crate) use index::{index_file_path, write_index_atomic};
pub use index::{
    read_index, resolve_latest_session_in_working_dir, resolve_session, sum_usage_from_events,
};
pub use index_entry::{ResumeFidelity, SessionIndexEntry, SessionRecordOrigin, SessionStatus};
#[cfg(test)]
pub(crate) use io::append_events;
#[cfg(test)]
pub(crate) use io::read_session_events;
pub use io::{RESERVED_SESSION_ID_STEMS, is_reserved_session_id, read_session_events_for_entry};
#[cfg(test)]
pub(crate) use io::{resolved_session_file_path, session_file_path};
pub use replay::ReplayArtifacts;
pub(crate) use timeline_lock::LockedTimelineFile;
pub use types::{SESSION_FORMAT_VERSION, SessionFileHeader, SessionPersistError};
