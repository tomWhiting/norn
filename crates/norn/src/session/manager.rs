//! One front door for the session lifecycle: [`SessionManager`].
//!
//! Every way a persisted session comes to life — create, resume, fork,
//! idempotent open-or-resume — goes through the manager and comes back
//! as an [`OpenSession`]: an [`EventStore`] with a write-through,
//! index-registered JSONL sink already installed, the session's
//! [`SessionIndexEntry`], and a [`ReplaySummary`] describing what was
//! recovered from disk. Callers never hand-assemble the
//! create-then-attach or resume-then-attach sequences the pre-Phase-2
//! free functions required.
//!
//! The manager composes the persistence engine, it does not replace it:
//! the tolerant reader ([`read_session_events`]), torn-line healing,
//! duplicate-`EventId` tolerance, retry-safe appends, batched index
//! maintenance with `EventStore::checkpoint`, and resume-time index
//! self-healing are all the Phase 1 contracts, unchanged.
//!
//! # Durability
//!
//! Every opening method takes the [`DurabilityPolicy`] explicitly — the
//! policy is the embedder's call per open, and there is no built-in
//! default.
//!
//! # Idempotency (`open_or_resume`)
//!
//! [`SessionManager::open_or_resume`] is the primitive for retry-safe
//! embedders (e.g. a durable-workflow activity that may run the same
//! logical step several times): the caller supplies the session ID — a
//! deterministic key derived from the unit of work, not a fresh UUID —
//! and the manager either creates the session under exactly that ID or
//! resumes the one a previous attempt left behind. The
//! check-and-insert runs under the inter-process index lock, so two
//! processes racing the same key converge on a single session instead
//! of minting duplicates. A retry therefore continues its predecessor's
//! event history rather than polluting the index with one session per
//! attempt.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::{DurabilityPolicy, EventStore, JsonlSink};

use super::persistence::index::{
    append_index_entry, insert_index_entry_if_absent, read_index, remove_index_entry,
    resolve_session, sum_usage_from_events, update_index_entry,
};
use super::persistence::io::{append_events, read_session_events, session_file_path};
use super::persistence::types::{
    SESSION_FORMAT_VERSION, SessionFileRead, SessionIndexEntry, SessionPersistError, SessionStatus,
};

/// Caller-supplied metadata recorded in the index entry of a newly
/// created session (also the create arm of
/// [`SessionManager::open_or_resume`] and the new entry minted by
/// [`SessionManager::fork`]).
#[derive(Clone, Debug)]
pub struct CreateSessionOptions {
    /// Model identifier active when the session is created.
    pub model: String,
    /// Working directory the session runs in. The manager records what
    /// it is given — deriving it (e.g. from the process CWD) is the
    /// caller's decision, never an assumed default.
    pub working_dir: String,
    /// Optional human-readable name, resolvable via
    /// [`SessionManager::resolve`].
    pub name: Option<String>,
}

/// What was recovered from disk while opening a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplaySummary {
    /// Events recovered from the session file and pre-loaded into the
    /// returned store. `0` for a freshly created session. For a fork
    /// this counts the copied source events plus the appended `Fork`
    /// marker.
    pub replayed_events: usize,
    /// Non-empty lines the tolerant reader skipped: torn writes,
    /// invalid JSON, unknown variants from a newer writer, and
    /// duplicate-`EventId` crash-retry artifacts. `0` for a healthy
    /// file (and always `0` for a fresh create). Surface or log a
    /// non-zero count — it means the replayed history is incomplete.
    /// For a fork it refers to the *source* session's file.
    pub skipped_lines: u64,
}

/// A session opened through [`SessionManager`]: the ready-to-append
/// store, its index entry, and the replay summary.
#[derive(Debug)]
pub struct OpenSession {
    /// Event store with a write-through, index-registered JSONL sink
    /// installed: every append persists to `{data_dir}/{id}.jsonl` and
    /// keeps the index entry (event count, usage totals, `updated_at`)
    /// in step per the chosen [`DurabilityPolicy`]. Call
    /// [`EventStore::checkpoint`] at turn boundaries to flush deferred
    /// index deltas; resume self-heals anything a crash leaves stale.
    pub store: EventStore,
    /// The session's index entry. On resume this carries the
    /// recomputed (self-healed) event count and usage totals, never
    /// stale values.
    pub entry: SessionIndexEntry,
    /// What was recovered from disk to populate [`Self::store`].
    pub replay: ReplaySummary,
}

/// One coherent owner of a session data directory.
///
/// Construct with an **explicit** directory — the manager never guesses
/// a home location (resolve a default via
/// `norn::config::paths::session_data_dir()` if you want the standard
/// one). Cheap to clone; holds no open handles itself (each
/// [`OpenSession`]'s sink owns its own file handle).
#[derive(Clone, Debug)]
pub struct SessionManager {
    data_dir: PathBuf,
}

impl SessionManager {
    /// Create a manager over `data_dir`. The directory (and the index
    /// inside it) is created lazily on first write.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// The data directory this manager owns.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Create a fresh session: mint a UUID v7 ID, register it in the
    /// index, and return a sink-equipped store ready for its first
    /// append (the session file is created immediately, with its
    /// version-header line).
    ///
    /// # Errors
    ///
    /// Propagates index-append and session-file-open failures.
    /// Persistence is never silently degraded to memory-only: a failed
    /// create returns no store. If the failure strikes after the index
    /// entry landed, the entry remains (harmless — it resumes as an
    /// empty session) and the error is still returned.
    pub fn create(
        &self,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let entry = new_index_entry(Uuid::now_v7().to_string(), options);
        append_index_entry(&self.data_dir, &entry)?;
        self.open_fresh(entry, durability)
    }

    /// Resume a persisted session: resolve `id_or_name` (empty string =
    /// most recently updated session, else exact ID, exact name, or a
    /// unique ID prefix of at least 8 characters), tolerantly replay
    /// its event file, self-heal a drifted index entry, and return the
    /// store with a sink attached for continued appends.
    ///
    /// Corrupt, unknown, and duplicate-`EventId` lines are skipped and
    /// counted in [`ReplaySummary::skipped_lines`]; a torn final line
    /// never prevents resume. The returned entry always carries the
    /// recomputed event count and usage totals (a failed index repair
    /// is logged, never fatal).
    ///
    /// # Errors
    ///
    /// [`SessionPersistError::NotFound`] /
    /// [`SessionPersistError::AmbiguousPrefix`] from resolution, and
    /// I/O errors reading the session file or opening the sink.
    pub fn resume(
        &self,
        id_or_name: &str,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let entry = resolve_session(&self.data_dir, id_or_name)?;
        self.resume_entry(entry, durability)
    }

    /// Fork a session: copy every recovered source event into a brand
    /// new session (durable batch append), append a `Fork` event whose
    /// `source_event_id` is the source's last event, and return the new
    /// session's sink-equipped store pre-loaded with the copied history.
    ///
    /// `source` resolves like [`Self::resume`]. `options` populates the
    /// new index entry — callers typically pass the source session's
    /// model and working directory but may override.
    /// [`ReplaySummary::skipped_lines`] reports tolerant-reader skips in
    /// the **source** file; the fork contains only the recovered events.
    ///
    /// # Errors
    ///
    /// Resolution errors for `source`,
    /// [`SessionPersistError::EmptySource`] when the source has no
    /// recoverable events, and I/O errors creating or opening the new
    /// session.
    pub fn fork(
        &self,
        source: &str,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let source_entry = resolve_session(&self.data_dir, source)?;
        let read = read_session_events(&self.data_dir, &source_entry.id)?;
        let last_event_id = read
            .events
            .last()
            .ok_or_else(|| SessionPersistError::EmptySource {
                id: source_entry.id.clone(),
            })?
            .base()
            .id
            .clone();

        let new_entry = new_index_entry(Uuid::now_v7().to_string(), options);
        append_index_entry(&self.data_dir, &new_entry)?;

        let fork_event = SessionEvent::Fork {
            base: EventBase::new(Some(last_event_id.clone())),
            source_event_id: last_event_id,
            forked_session_id: new_entry.id.clone(),
        };
        let mut events = read.events;
        events.push(fork_event);
        append_events(&self.data_dir, &new_entry.id, &events, false)?;

        // Re-resolve so the returned entry carries the event count and
        // usage totals the batch append just recorded.
        let entry = resolve_session(&self.data_dir, &new_entry.id)?;
        let sink = JsonlSink::open_registered(&self.data_dir, &entry.id, durability)?;
        let replay = ReplaySummary {
            replayed_events: events.len(),
            skipped_lines: read.skipped_lines,
        };
        Ok(OpenSession {
            store: EventStore::with_sink_and_events(Box::new(sink), events),
            entry,
            replay,
        })
    }

    /// Idempotently open the session with the **caller-supplied** ID
    /// `id`: create it (recording `options`) when the index has no
    /// entry with that exact ID, otherwise resume the existing session
    /// — `options` is ignored on the resume arm because the entry
    /// already carries its metadata.
    ///
    /// This is the retry-safe primitive for embedders that derive the
    /// ID deterministically from a unit of work (e.g. a workflow run +
    /// activity key): every attempt converges on one session and one
    /// index entry. The existence check and the insert hold the
    /// inter-process index lock together, so concurrent callers with
    /// the same ID race safely — exactly one creates, the rest resume.
    /// A crash at any point retries cleanly: an entry without a file
    /// resumes as an empty session, and the tolerant reader absorbs
    /// torn or duplicated lines from interrupted attempts.
    ///
    /// Matching is by exact ID only — never by name or ID prefix — so
    /// an idempotency key can never silently attach to someone else's
    /// session.
    ///
    /// # Errors
    ///
    /// [`SessionPersistError::InvalidSessionId`] when `id` is empty,
    /// does not start with an ASCII letter or digit, contains characters
    /// outside `[A-Za-z0-9._-]` (the ID becomes the `{id}.jsonl` file
    /// name, so anything path-capable is rejected at this boundary), or
    /// is reserved by the persistence layer (`"index"` and the rest of
    /// the `index.*` family name the session index, its lock, and its
    /// rewrite staging files — matched case-insensitively because the
    /// default macOS / Windows filesystems are); plus the same I/O errors
    /// as [`Self::create`] / [`Self::resume`].
    pub fn open_or_resume(
        &self,
        id: &str,
        options: CreateSessionOptions,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        validate_explicit_session_id(id)?;
        let candidate = new_index_entry(id.to_owned(), options);
        match insert_index_entry_if_absent(&self.data_dir, &candidate)? {
            Some(existing) => self.resume_entry(existing, durability),
            None => self.open_fresh(candidate, durability),
        }
    }

    /// List every session in the index, in file order. Corrupt index
    /// lines are skipped with a warning by the tolerant index reader —
    /// one bad entry never makes the rest unlistable.
    ///
    /// # Errors
    ///
    /// Fails only when the index file itself is unreadable.
    pub fn list(&self) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
        read_index(&self.data_dir)
    }

    /// Resolve `id_or_name` (empty = most recently updated, exact ID,
    /// exact name, or unique >= 8-character ID prefix) to its index
    /// entry without opening the session.
    ///
    /// # Errors
    ///
    /// [`SessionPersistError::NotFound`] /
    /// [`SessionPersistError::AmbiguousPrefix`].
    pub fn resolve(&self, id_or_name: &str) -> Result<SessionIndexEntry, SessionPersistError> {
        resolve_session(&self.data_dir, id_or_name)
    }

    /// Read a session's events without opening it for appending (the
    /// export / inspection path): resolve `id_or_name`, then tolerantly
    /// read the event file. Returns the resolved entry alongside the
    /// [`SessionFileRead`] so callers can render metadata and surface
    /// [`SessionFileRead::skipped_lines`].
    ///
    /// # Errors
    ///
    /// Resolution errors, and I/O errors reading the file as a whole
    /// (individual corrupt lines are skipped, not fatal).
    pub fn read_events(
        &self,
        id_or_name: &str,
    ) -> Result<(SessionIndexEntry, SessionFileRead), SessionPersistError> {
        let entry = resolve_session(&self.data_dir, id_or_name)?;
        let read = read_session_events(&self.data_dir, &entry.id)?;
        Ok((entry, read))
    }

    /// Rename a session: resolve `id_or_name`, then set (or clear with
    /// `None`) the human-readable name on its index entry under the
    /// inter-process lock. Returns the updated entry.
    ///
    /// # Errors
    ///
    /// Resolution errors and index-rewrite failures.
    pub fn rename(
        &self,
        id_or_name: &str,
        name: Option<String>,
    ) -> Result<SessionIndexEntry, SessionPersistError> {
        let mut entry = resolve_session(&self.data_dir, id_or_name)?;
        let applied = name.clone();
        update_index_entry(&self.data_dir, &entry.id, move |e| e.name = name)?;
        entry.name = applied;
        Ok(entry)
    }

    /// Delete a session: resolve `id_or_name`, remove its event file
    /// (a missing file is fine — the session may never have appended),
    /// then remove its index entry under the inter-process lock.
    /// Returns the removed entry.
    ///
    /// # Errors
    ///
    /// Resolution errors, a file-removal failure (the index entry is
    /// left intact so the session is not orphaned while its file
    /// lingers), and index-rewrite failures.
    pub fn delete(&self, id_or_name: &str) -> Result<SessionIndexEntry, SessionPersistError> {
        let entry = resolve_session(&self.data_dir, id_or_name)?;
        let path = session_file_path(&self.data_dir, &entry.id);
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(SessionPersistError::Io(std::io::Error::new(
                error.kind(),
                format!("failed to delete session file {}: {error}", path.display()),
            )));
        }
        remove_index_entry(&self.data_dir, &entry.id)?;
        Ok(entry)
    }

    /// Open a sink-equipped, empty store for an entry that was just
    /// inserted into the index (create / open-or-resume create arm).
    fn open_fresh(
        &self,
        entry: SessionIndexEntry,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let sink = JsonlSink::open_registered(&self.data_dir, &entry.id, durability)?;
        Ok(OpenSession {
            store: EventStore::with_sink(Box::new(sink)),
            entry,
            replay: ReplaySummary::default(),
        })
    }

    /// Replay an already-resolved entry's event file into a
    /// sink-equipped store, self-healing the index entry on drift.
    fn resume_entry(
        &self,
        entry: SessionIndexEntry,
        durability: DurabilityPolicy,
    ) -> Result<OpenSession, SessionPersistError> {
        let read = read_session_events(&self.data_dir, &entry.id)?;
        let entry = reconcile_index_entry(&self.data_dir, entry, &read.events);
        let sink = JsonlSink::open_registered(&self.data_dir, &entry.id, durability)?;
        let replay = ReplaySummary {
            replayed_events: read.events.len(),
            skipped_lines: read.skipped_lines,
        };
        Ok(OpenSession {
            store: EventStore::with_sink_and_events(Box::new(sink), read.events),
            entry,
            replay,
        })
    }
}

/// Build a fresh [`SessionIndexEntry`] from caller options.
fn new_index_entry(id: String, options: CreateSessionOptions) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id,
        name: options.name,
        model: options.model,
        working_dir: options.working_dir,
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
    }
}

/// Validate a caller-supplied session ID at the open-or-resume boundary.
/// The ID becomes the `{id}.jsonl` file name, so it must be non-empty,
/// start with an ASCII letter or digit (rules out `.`, `..`, and hidden
/// files), contain only `[A-Za-z0-9._-]` (rules out path separators on
/// every platform), and not be reserved by the persistence layer (rules
/// out ids whose file would be a persistence-owned file — `"index"` would
/// name `index.jsonl`, the session index itself; see
/// [`super::persistence::io::is_reserved_session_id`]).
fn validate_explicit_session_id(id: &str) -> Result<(), SessionPersistError> {
    let invalid = |reason: &str| SessionPersistError::InvalidSessionId {
        id: id.to_owned(),
        reason: reason.to_owned(),
    };
    let Some(first) = id.chars().next() else {
        return Err(invalid("must not be empty"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(invalid("must start with an ASCII letter or digit"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(invalid(
            "may contain only ASCII letters, digits, '-', '_', and '.'",
        ));
    }
    super::persistence::io::ensure_session_id_not_reserved(id)
}

/// Compare `entry`'s `event_count` and usage totals against the events
/// actually recovered from the session file and repair the index entry
/// when they drifted (the crash-staleness window the batched index
/// maintenance accepts by design). Returns the entry with the
/// recomputed values; a failed repair write is logged at error level
/// and the recomputed (in-memory) values are still returned so the
/// caller never sees stale numbers.
fn reconcile_index_entry(
    data_dir: &Path,
    entry: SessionIndexEntry,
    events: &[SessionEvent],
) -> SessionIndexEntry {
    let actual_count = u64::try_from(events.len()).unwrap_or(u64::MAX);
    let actual_usage = sum_usage_from_events(events);
    if entry.event_count == actual_count
        && entry.total_input_tokens == actual_usage.input_tokens
        && entry.total_output_tokens == actual_usage.output_tokens
        && entry.total_cache_read_tokens == actual_usage.cache_read_tokens
    {
        return entry;
    }
    tracing::warn!(
        session_id = %entry.id,
        indexed_count = entry.event_count,
        actual_count,
        "session index entry drifted from the event file (crash before \
         a deferred index delta landed, or a failed index update after \
         a durable append); repairing",
    );
    let mut repaired = entry;
    repaired.event_count = actual_count;
    repaired.total_input_tokens = actual_usage.input_tokens;
    repaired.total_output_tokens = actual_usage.output_tokens;
    repaired.total_cache_read_tokens = actual_usage.cache_read_tokens;
    if let Err(error) = update_index_entry(data_dir, &repaired.id, |e| {
        e.event_count = actual_count;
        e.total_input_tokens = actual_usage.input_tokens;
        e.total_output_tokens = actual_usage.output_tokens;
        e.total_cache_read_tokens = actual_usage.cache_read_tokens;
    }) {
        tracing::error!(
            session_id = %repaired.id,
            %error,
            "failed to persist the repaired session index entry; resume \
             continues with the recomputed values",
        );
    }
    repaired
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::session::events::EventUsage;
    use crate::session::persistence::index::index_file_path;

    fn options(model: &str) -> CreateSessionOptions {
        CreateSessionOptions {
            model: model.to_owned(),
            working_dir: "/work".to_owned(),
            name: None,
        }
    }

    fn user_msg(text: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: text.to_owned(),
        }
    }

    fn assistant_with_usage(input: u64, output: u64, cache_read: u64) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: "ok".to_owned(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            usage: EventUsage {
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: cache_read,
                cache_write_tokens: 0,
                cost_usd: None,
            },
            stop_reason: "stop".to_owned(),
            response_id: None,
        }
    }

    #[test]
    fn create_returns_indexed_sink_registered_store() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt-x"), DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(opened.replay, ReplaySummary::default());
        assert_eq!(opened.entry.model, "gpt-x");
        assert_eq!(opened.entry.format_version, SESSION_FORMAT_VERSION);

        // Indexed immediately.
        let listed = manager.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, opened.entry.id);

        // The store writes through and the registered sink maintains
        // the index at checkpoint.
        opened.store.append(user_msg("hello")).unwrap();
        opened.store.append(assistant_with_usage(7, 3, 1)).unwrap();
        opened.store.checkpoint().unwrap();
        let read = read_session_events(tmp.path(), &opened.entry.id).unwrap();
        assert_eq!(read.events.len(), 2);
        let listed = manager.list().unwrap();
        assert_eq!(listed[0].event_count, 2);
        assert_eq!(listed[0].total_input_tokens, 7);
        assert_eq!(listed[0].total_output_tokens, 3);
        assert_eq!(listed[0].total_cache_read_tokens, 1);
    }

    #[test]
    fn create_honors_name_and_resolve_finds_it() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "gpt".to_owned(),
                    working_dir: "/w".to_owned(),
                    name: Some("nightly".to_owned()),
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();
        let resolved = manager.resolve("nightly").unwrap();
        assert_eq!(resolved.id, opened.entry.id);
    }

    #[test]
    fn resume_replays_events_with_clean_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let id = opened.entry.id.clone();
        opened.store.append(user_msg("one")).unwrap();
        opened.store.append(user_msg("two")).unwrap();
        drop(opened);

        let resumed = manager.resume(&id, DurabilityPolicy::Flush).unwrap();
        assert_eq!(resumed.replay.replayed_events, 2);
        assert_eq!(resumed.replay.skipped_lines, 0);
        assert_eq!(resumed.store.len(), 2);
        assert_eq!(resumed.entry.id, id);

        // Continued appends land in the same file.
        resumed.store.append(user_msg("three")).unwrap();
        drop(resumed);
        let read = read_session_events(tmp.path(), &id).unwrap();
        assert_eq!(read.events.len(), 3);
    }

    /// Regression: the pre-manager resume path dropped the tolerant
    /// reader's skip count on the floor — the caller had no way to know
    /// the replayed history was incomplete.
    #[test]
    fn resume_surfaces_skipped_lines() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let id = opened.entry.id.clone();
        opened.store.append(user_msg("intact")).unwrap();
        drop(opened);

        // Tear the file the way ENOSPC / `kill -9` would.
        let path = session_file_path(tmp.path(), &id);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"type":"user_message","content":"tor"#)
            .unwrap();
        drop(file);

        let resumed = manager.resume(&id, DurabilityPolicy::Flush).unwrap();
        assert_eq!(resumed.replay.replayed_events, 1);
        assert_eq!(
            resumed.replay.skipped_lines, 1,
            "the torn line must be surfaced to the caller",
        );
    }

    #[test]
    fn resume_self_heals_drifted_index_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let id = opened.entry.id.clone();
        opened.store.append(user_msg("one")).unwrap();
        opened.store.append(assistant_with_usage(10, 5, 2)).unwrap();
        opened.store.checkpoint().unwrap();
        drop(opened);

        // Simulate crash staleness: zero the entry behind the manager's
        // back.
        update_index_entry(tmp.path(), &id, |e| {
            e.event_count = 0;
            e.total_input_tokens = 0;
            e.total_output_tokens = 0;
            e.total_cache_read_tokens = 0;
        })
        .unwrap();

        let resumed = manager.resume(&id, DurabilityPolicy::Flush).unwrap();
        assert_eq!(resumed.entry.event_count, 2);
        assert_eq!(resumed.entry.total_input_tokens, 10);
        assert_eq!(resumed.entry.total_output_tokens, 5);
        assert_eq!(resumed.entry.total_cache_read_tokens, 2);
        let listed = manager.list().unwrap();
        assert_eq!(listed[0].event_count, 2, "repair persisted to disk");
    }

    #[test]
    fn fork_copies_events_appends_marker_and_attaches_sink() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let source = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let source_id = source.entry.id.clone();
        source.store.append(user_msg("one")).unwrap();
        source.store.append(user_msg("two")).unwrap();
        let last_id = source.store.last_event_id().unwrap();
        drop(source);

        let fork = manager
            .fork(&source_id, options("gpt-fork"), DurabilityPolicy::Flush)
            .unwrap();
        assert_ne!(fork.entry.id, source_id);
        assert_eq!(fork.entry.model, "gpt-fork");
        assert_eq!(fork.replay.replayed_events, 3, "2 copied + Fork marker");
        assert_eq!(fork.store.len(), 3);
        assert_eq!(
            fork.entry.event_count, 3,
            "returned entry reflects the batch append",
        );
        match fork.store.events().last().unwrap() {
            SessionEvent::Fork {
                source_event_id,
                forked_session_id,
                ..
            } => {
                assert_eq!(source_event_id, &last_id);
                assert_eq!(forked_session_id, &fork.entry.id);
            }
            other => panic!("expected Fork tail, got {other:?}"),
        }

        // The fork's sink is live: an append after forking persists.
        let fork_id = fork.entry.id.clone();
        fork.store.append(user_msg("post-fork")).unwrap();
        drop(fork);
        let read = read_session_events(tmp.path(), &fork_id).unwrap();
        assert_eq!(read.events.len(), 4);

        // Source file untouched.
        let source_read = read_session_events(tmp.path(), &source_id).unwrap();
        assert_eq!(source_read.events.len(), 2);
    }

    #[test]
    fn fork_empty_source_returns_empty_source() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let source = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let err = manager
            .fork(&source.entry.id, options("gpt"), DurabilityPolicy::Flush)
            .unwrap_err();
        assert!(matches!(err, SessionPersistError::EmptySource { .. }));
    }

    #[test]
    fn open_or_resume_creates_with_caller_supplied_id() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .open_or_resume("wf-1234.step-2", options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(opened.entry.id, "wf-1234.step-2");
        assert_eq!(opened.replay, ReplaySummary::default());
        opened.store.append(user_msg("first attempt")).unwrap();
        drop(opened);

        let listed = manager.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "wf-1234.step-2");
    }

    /// The idempotency contract: a retry with the same deterministic key
    /// resumes the prior attempt's session — same ID, same history, one
    /// index entry — instead of minting a new session per attempt.
    #[test]
    fn open_or_resume_retry_resumes_prior_session() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let first = manager
            .open_or_resume("wf-77.activity-3", options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        first.store.append(user_msg("attempt one")).unwrap();
        drop(first);

        let retry = manager
            .open_or_resume("wf-77.activity-3", options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(retry.entry.id, "wf-77.activity-3");
        assert_eq!(retry.replay.replayed_events, 1);
        assert_eq!(retry.store.len(), 1, "prior history replayed");
        drop(retry);

        assert_eq!(manager.list().unwrap().len(), 1, "no duplicate entry");
    }

    /// Crash between index insert and first event: the entry exists but
    /// the file does not. The retry must resume cleanly as an empty
    /// session, not error or duplicate.
    #[test]
    fn open_or_resume_recovers_entry_without_file() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        // Insert the index entry directly — the "crashed before the sink
        // opened" state.
        let entry = new_index_entry("wf-crash".to_owned(), options("gpt"));
        append_index_entry(tmp.path(), &entry).unwrap();
        assert!(!session_file_path(tmp.path(), "wf-crash").exists());

        let opened = manager
            .open_or_resume("wf-crash", options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(opened.entry.id, "wf-crash");
        assert_eq!(opened.replay.replayed_events, 0);
        opened.store.append(user_msg("recovered")).unwrap();
        drop(opened);
        let read = read_session_events(tmp.path(), "wf-crash").unwrap();
        assert_eq!(read.events.len(), 1);
        assert_eq!(manager.list().unwrap().len(), 1);
    }

    #[test]
    fn open_or_resume_matches_exact_id_never_name_or_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        // A session *named* "alpha" with a random UUID id.
        manager
            .create(
                CreateSessionOptions {
                    model: "gpt".to_owned(),
                    working_dir: "/w".to_owned(),
                    name: Some("alpha".to_owned()),
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();

        // open_or_resume("alpha") must NOT attach to the named session —
        // it creates a new one whose ID is literally "alpha".
        let opened = manager
            .open_or_resume("alpha", options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(opened.entry.id, "alpha");
        assert_eq!(opened.replay.replayed_events, 0);
        assert_eq!(manager.list().unwrap().len(), 2);
    }

    #[test]
    fn open_or_resume_rejects_path_capable_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        for bad in [
            "",
            ".",
            "..",
            "../evil",
            "a/b",
            "a\\b",
            ".hidden",
            "-rf",
            "id with space",
            "id:colon",
        ] {
            let err = manager
                .open_or_resume(bad, options("gpt"), DurabilityPolicy::Flush)
                .unwrap_err();
            assert!(
                matches!(err, SessionPersistError::InvalidSessionId { .. }),
                "id {bad:?} must be rejected, got {err:?}",
            );
        }
        assert!(
            manager.list().unwrap().is_empty(),
            "rejected ids must leave no index entries",
        );
    }

    /// Blocker regression: session IDs map to `{id}.jsonl`, so the id
    /// `"index"` mapped onto `{data_dir}/index.jsonl` — the shared session
    /// index. `open_or_resume("index", ...)` appended session events into
    /// the index and `delete("index")` destroyed it for every session.
    /// The whole reserved name family must be rejected at validation.
    #[test]
    fn open_or_resume_rejects_reserved_persistence_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        // A real session first, so the index file exists and corruption
        // would be observable.
        let existing = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let existing_id = existing.entry.id.clone();
        drop(existing);

        for reserved in ["index", "index.lock", "index.jsonl", "index.jsonl.tmp.0"] {
            let err = manager
                .open_or_resume(reserved, options("gpt"), DurabilityPolicy::Flush)
                .unwrap_err();
            assert!(
                matches!(err, SessionPersistError::InvalidSessionId { .. }),
                "reserved id {reserved:?} must be rejected, got {err:?}",
            );
        }

        // Near-misses outside the dotted family stay valid.
        let opened = manager
            .open_or_resume("indexer-1", options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(opened.entry.id, "indexer-1");
        drop(opened);

        // The index itself was never written to as a session file: both
        // legitimate sessions are still listed, nothing else.
        let mut ids: Vec<String> = manager.list().unwrap().into_iter().map(|e| e.id).collect();
        ids.sort();
        let mut expected = vec![existing_id, "indexer-1".to_owned()];
        expected.sort();
        assert_eq!(ids, expected);
    }

    /// `delete("index")` must never be able to remove the session index.
    #[test]
    fn delete_can_never_destroy_the_index() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        drop(opened);

        let err = manager.delete("index").unwrap_err();
        assert!(
            !matches!(err, SessionPersistError::Io(_)),
            "delete(\"index\") must fail by rejection, not by touching files: {err:?}",
        );
        assert!(
            index_file_path(tmp.path()).exists(),
            "the session index file must survive",
        );
        assert_eq!(manager.list().unwrap().len(), 1, "the index is intact");
    }

    /// Defense in depth: a reserved ID can only enter the index through a
    /// hand-edited file (every programmatic insertion path rejects it).
    /// Even then, resolution must refuse to route session I/O onto the
    /// persistence layer's own files.
    #[test]
    fn reserved_id_smuggled_into_index_is_unreachable() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        drop(opened);

        // Bypass every guard: write the index line by hand.
        let smuggled = new_index_entry("index".to_owned(), options("gpt"));
        let line = serde_json::to_string(&smuggled).unwrap();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(index_file_path(tmp.path()))
            .unwrap();
        writeln!(file, "{line}").unwrap();
        drop(file);

        for (what, err) in [
            ("resolve", manager.resolve("index").unwrap_err()),
            (
                "resume",
                manager
                    .resume("index", DurabilityPolicy::Flush)
                    .unwrap_err(),
            ),
            ("delete", manager.delete("index").unwrap_err()),
            ("read_events", manager.read_events("index").unwrap_err()),
        ] {
            assert!(
                matches!(err, SessionPersistError::InvalidSessionId { .. }),
                "{what}(\"index\") must reject the smuggled reserved id, got {err:?}",
            );
        }
        assert!(
            index_file_path(tmp.path()).exists(),
            "the index file must survive every attempt",
        );
    }

    /// Two callers racing the same deterministic key (the multi-process
    /// topology, simulated with threads — the index lock excludes both)
    /// must converge on exactly one session.
    #[test]
    fn open_or_resume_concurrent_same_id_converges_on_one_session() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let manager = SessionManager::new(dir);
                    let opened = manager
                        .open_or_resume(
                            "wf-race.key",
                            CreateSessionOptions {
                                model: "gpt".to_owned(),
                                working_dir: "/w".to_owned(),
                                name: None,
                            },
                            DurabilityPolicy::Flush,
                        )
                        .unwrap();
                    opened
                        .store
                        .append(SessionEvent::UserMessage {
                            base: EventBase::new(None),
                            content: format!("from-{i}"),
                        })
                        .unwrap();
                    opened.entry.id
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), "wf-race.key");
        }
        let manager = SessionManager::new(tmp.path());
        assert_eq!(manager.list().unwrap().len(), 1, "one entry, no dupes");
        let read = read_session_events(tmp.path(), "wf-race.key").unwrap();
        assert_eq!(read.events.len(), 4, "every caller's append landed");
    }

    #[test]
    fn rename_sets_and_clears_index_name() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let id = opened.entry.id.clone();
        drop(opened);

        let renamed = manager.rename(&id, Some("milestone".to_owned())).unwrap();
        assert_eq!(renamed.name.as_deref(), Some("milestone"));
        assert_eq!(
            manager.resolve("milestone").unwrap().id,
            id,
            "rename must persist to the index",
        );

        let cleared = manager.rename(&id, None).unwrap();
        assert_eq!(cleared.name, None);
        assert!(
            matches!(
                manager.resolve("milestone").unwrap_err(),
                SessionPersistError::NotFound { .. },
            ),
            "cleared name must no longer resolve",
        );

        let err = manager
            .rename("missing-session", Some("x".to_owned()))
            .unwrap_err();
        assert!(matches!(err, SessionPersistError::NotFound { .. }));
    }

    #[test]
    fn delete_removes_file_and_index_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let id = opened.entry.id.clone();
        opened.store.append(user_msg("doomed")).unwrap();
        drop(opened);

        let removed = manager.delete(&id).unwrap();
        assert_eq!(removed.id, id);
        assert!(!session_file_path(tmp.path(), &id).exists());
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn delete_tolerates_missing_session_file() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        // Index entry with no file (never appended, file removed by hand).
        let entry = new_index_entry("ghost-file".to_owned(), options("gpt"));
        append_index_entry(tmp.path(), &entry).unwrap();
        let removed = manager.delete("ghost-file").unwrap();
        assert_eq!(removed.id, "ghost-file");
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn delete_unknown_session_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let err = manager.delete("nonexistent").unwrap_err();
        assert!(matches!(err, SessionPersistError::NotFound { .. }));
    }

    #[test]
    fn read_events_returns_entry_and_tolerant_read() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(options("gpt"), DurabilityPolicy::Flush)
            .unwrap();
        let id = opened.entry.id.clone();
        opened.store.append(user_msg("exported")).unwrap();
        drop(opened);

        let (entry, read) = manager.read_events(&id).unwrap();
        assert_eq!(entry.id, id);
        assert_eq!(read.events.len(), 1);
        assert_eq!(read.skipped_lines, 0);

        let err = manager.read_events("nope-not-here").unwrap_err();
        assert!(matches!(err, SessionPersistError::NotFound { .. }));
    }

    /// A failed open must never silently degrade to memory-only
    /// persistence: occupy the session file path with a directory so
    /// the sink open fails.
    #[test]
    fn open_failure_surfaces_instead_of_degrading() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        fs::create_dir_all(session_file_path(tmp.path(), "occupied")).unwrap();
        let result = manager.open_or_resume("occupied", options("gpt"), DurabilityPolicy::Flush);
        assert!(result.is_err(), "open failure must not be swallowed");
    }
}
