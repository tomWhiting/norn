//! Re-exports for JSONL-backed session persistence.
//!
//! The implementation lives in `norn::session` so CLI, TUI, and workflow
//! consumers all use the same session index and JSONL storage. The
//! lifecycle (create / resume / fork / open-or-resume / list / delete)
//! goes through [`SessionManager`]; the remaining re-exports are the
//! engine primitives the CLI's command surface and tests still touch
//! directly.

#[cfg(test)]
pub(crate) use norn::session::read_index;
pub(crate) use norn::session::{
    CreateSessionOptions, SessionIndexEntry, SessionManager, SessionPersistError, SessionStatus,
};

#[cfg(test)]
pub(crate) fn session_file_path(
    data_dir: &std::path::Path,
    session_id: &str,
) -> std::path::PathBuf {
    data_dir.join(format!("{session_id}.jsonl"))
}
