//! Full-fidelity spool for oversized tool outputs.
//!
//! The persisted [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
//! carries the bounded model-facing projection of a tool's output. Before
//! this module existed, an over-budget output was *replaced* by that
//! projection and the full payload was discarded from the durable log
//! (session-fidelity inventory, Gap 5). The spool closes that gap: the
//! full output is written **verbatim** — no size cap, no compression —
//! to `spool/` under the OWNING ROOT session's sibling directory (the
//! ruled storage layout: `<data-dir>/<root-id>.jsonl` next to
//! `<data-dir>/<root-id>/` containing `children/` and `spool/`). Child
//! sessions spool into the same root-keyed directory their timeline
//! lives under — `SessionManager` and the branching authority both
//! derive the key from the owning root, so a child spools to one place
//! whether freshly minted or later resumed. The `ToolResult` event
//! carries a durable [`spool reference`](SpoolWriter::write) alongside
//! the capped projection.
//!
//! # Durability discipline
//!
//! Spool writes follow the same write-through-before-memory discipline as
//! the primary event log: the spool file is fully handed to the OS (and
//! fsynced — file **and** the directory-entry chain naming it — when the
//! session's [`DurabilityPolicy`] fsyncs event lines) **before** the
//! referencing event is appended. A durable event can
//! therefore never reference a spool file that was not at least written
//! through; a crash between the two leaves an unreferenced orphan file,
//! never a dangling reference (under [`DurabilityPolicy::Flush`] an OS
//! crash shares the primary log's page-cache loss window — an
//! owner-chosen trade, not this module's).
//!
//! # Reference format
//!
//! A spool reference is relative to the session **data directory**:
//! `<root-session-id>/spool/<event-id>.bin` (the id component is the
//! owning ROOT's). Anchoring at the data directory
//! (rather than the owning session's directory) keeps references valid
//! when events are copied between stores under the same data directory
//! (fork seeding copies parent `ToolResult` events into child stores).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::events::EventId;
use crate::session::persistence::SessionPersistError;
use crate::session::persistence::io::ensure_session_id_path_safe;
use crate::session::store::DurabilityPolicy;
use crate::util::PrivateRoot;

/// Name of the spool subdirectory inside a session's sibling directory.
const SPOOL_DIR_NAME: &str = "spool";

/// File extension for spooled payloads (verbatim serialized JSON bytes).
const SPOOL_FILE_EXTENSION: &str = "bin";

/// Writes full-size tool outputs into a session's `spool/` directory.
///
/// Constructed by [`SessionManager`](crate::session::SessionManager) for
/// every store it opens and attached via
/// [`EventStore::attach_spool`](crate::session::store::EventStore::attach_spool).
/// Each write produces one immutable file named by the referencing
/// event's [`EventId`]; files are never rewritten, so concurrent writers
/// of *distinct* events need no locking.
#[derive(Debug)]
pub struct SpoolWriter {
    data_dir: PathBuf,
    session_id: String,
    fsync: bool,
}

impl SpoolWriter {
    /// Build a writer for `session_id`'s spool directory under
    /// `data_dir`, matching the durability of the session's event sink:
    /// policies that fsync event lines also fsync spool files, so a
    /// fsynced event can never outlive the payload it references across
    /// a power loss; [`DurabilityPolicy::Flush`] hands spool bytes to
    /// the OS exactly like event lines.
    #[must_use]
    pub fn for_session(data_dir: &Path, session_id: &str, durability: DurabilityPolicy) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            session_id: session_id.to_owned(),
            fsync: durability != DurabilityPolicy::Flush,
        }
    }

    /// Absolute path of this session's spool directory.
    #[must_use]
    pub fn spool_dir(&self) -> PathBuf {
        self.data_dir.join(&self.session_id).join(SPOOL_DIR_NAME)
    }

    /// Write `output` verbatim (its exact serialized JSON bytes) as the
    /// spool payload for the event `event_id`, creating the spool
    /// directory on first use. Returns the durable data-dir-relative
    /// reference (`<session-id>/spool/<event-id>.bin`) to record on the
    /// event.
    ///
    /// The file is fully written (and fsynced per the session's
    /// durability policy) before this returns, so callers appending the
    /// referencing event afterwards uphold the write-through ordering.
    /// Under an fsyncing policy the sync covers the **directory-entry
    /// chain** as well as the file: a file `sync_all` persists content
    /// and inode but not the parent directory's entry naming it, so
    /// `spool/`, the session directory, and the data directory are each
    /// synced after the file — otherwise a power loss could durably keep
    /// the referencing event (fsynced into the long-existing session
    /// file) while dropping the dirent of the payload it references.
    ///
    /// # Errors
    ///
    /// [`SessionPersistError::Serde`] when `output` cannot serialize
    /// (structurally impossible for values built from tool results, but
    /// never assumed), and [`SessionPersistError::Io`] when the
    /// directory cannot be created or the file cannot be written
    /// through. On error no reference exists, so the caller's event
    /// append must not proceed with a spool claim.
    pub fn write(&self, event_id: &EventId, output: &Value) -> Result<String, SessionPersistError> {
        let bytes = serde_json::to_vec(output)?;
        ensure_session_id_path_safe(&self.session_id)?;
        let _permit = crate::session::persistence::acquire_private_fs()?;
        let root = PrivateRoot::create(&self.data_dir)?;
        let session_dir = PathBuf::from(&self.session_id);
        let dir = session_dir.join(SPOOL_DIR_NAME);
        root.create_dir_all(&dir)?;
        let file_name = format!("{event_id}.{SPOOL_FILE_EXTENSION}");
        let path = dir.join(&file_name);
        let mut file = root.create_new(&path)?;
        file.write_all(&bytes)?;
        if self.fsync {
            file.sync_all()?;
            // Persist the directory entries that name the freshly written
            // file. `create_dir_all` may have minted any of these on this
            // very write, and even a pre-existing `spool/` holds a new
            // entry for the file itself; syncing the full chain keeps the
            // no-dangling-reference guarantee true across power loss. The
            // cost is three directory fsyncs, paid only on over-budget
            // outputs.
            root.sync_dir(&dir)?;
            root.sync_dir(&session_dir)?;
            root.sync_dir(Path::new(""))?;
        }
        Ok(format!("{}/{SPOOL_DIR_NAME}/{file_name}", self.session_id))
    }
}

/// Resolve a persisted spool reference to the full tool output it names.
///
/// This is the forensics/read side of the spool: given the session data
/// directory and the `spool_ref` recorded on a
/// [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult),
/// it returns the verbatim full output that was capped in the
/// model-facing projection.
///
/// # Errors
///
/// [`SessionPersistError::InvalidSpoolRef`] when the reference does not
/// have the exact `<session-id>/spool/<file>.bin` shape produced by
/// [`SpoolWriter::write`] (session files are parsed tolerantly, so a
/// hand-edited or corrupted reference must never traverse outside the
/// data directory); [`SessionPersistError::Io`] when the spool file
/// cannot be read; [`SessionPersistError::Serde`] when its bytes do not
/// parse back into a JSON value.
pub fn read_spooled_output(data_dir: &Path, spool_ref: &str) -> Result<Value, SessionPersistError> {
    let relative = validate_spool_ref(spool_ref)?;
    let _permit = crate::session::persistence::acquire_private_fs()?;
    let root = PrivateRoot::open(data_dir)?;
    let mut file = root.open_read(&relative)?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Validate `spool_ref` and resolve it to an absolute path under
/// `data_dir`.
///
/// # Errors
///
/// [`SessionPersistError::InvalidSpoolRef`] when the reference is not of
/// the exact `<session-id>/spool/<file>.bin` form with path-safe
/// components.
pub fn resolve_spool_ref(data_dir: &Path, spool_ref: &str) -> Result<PathBuf, SessionPersistError> {
    Ok(data_dir.join(validate_spool_ref(spool_ref)?))
}

fn validate_spool_ref(spool_ref: &str) -> Result<PathBuf, SessionPersistError> {
    let invalid = |reason: &str| SessionPersistError::InvalidSpoolRef {
        spool_ref: spool_ref.to_owned(),
        reason: reason.to_owned(),
    };
    let mut parts = spool_ref.split('/');
    let (Some(session_id), Some(spool_dir), Some(file_name), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid(
            "expected exactly three '/'-separated components \
             (<session-id>/spool/<file>)",
        ));
    };
    if spool_dir != SPOOL_DIR_NAME {
        return Err(invalid("middle component must be 'spool'"));
    }
    if !is_path_safe_component(session_id) {
        return Err(invalid(
            "session-id component must start with an ASCII letter or digit \
             and contain only [A-Za-z0-9._-]",
        ));
    }
    if !is_path_safe_component(file_name) {
        return Err(invalid(
            "file component must start with an ASCII letter or digit and \
             contain only [A-Za-z0-9._-]",
        ));
    }
    let Some(stem) = file_name.strip_suffix(&format!(".{SPOOL_FILE_EXTENSION}")) else {
        return Err(invalid("file component must end in '.bin'"));
    };
    if stem.is_empty() {
        return Err(invalid("file component must have a non-empty stem"));
    }
    Ok(PathBuf::from(session_id)
        .join(SPOOL_DIR_NAME)
        .join(file_name))
}

/// A component is path-safe when it cannot traverse or hide: non-empty,
/// leading ASCII alphanumeric (rules out `.`, `..`, and hidden files),
/// and only `[A-Za-z0-9._-]` throughout (rules out separators on every
/// platform). This is the same character discipline session IDs are
/// validated with at the open-or-resume boundary.
fn is_path_safe_component(component: &str) -> bool {
    let mut chars = component.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    component
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = SpoolWriter::for_session(tmp.path(), "sess-1", DurabilityPolicy::Flush);
        let event_id = EventId::new();
        let full = Value::String("x".repeat(200_000));

        let spool_ref = writer.write(&event_id, &full).unwrap();
        assert_eq!(spool_ref, format!("sess-1/spool/{event_id}.bin"));

        let on_disk = std::fs::read(resolve_spool_ref(tmp.path(), &spool_ref).unwrap()).unwrap();
        assert_eq!(
            on_disk,
            serde_json::to_vec(&full).unwrap(),
            "spool bytes must be the verbatim serialized output",
        );
        assert_eq!(read_spooled_output(tmp.path(), &spool_ref).unwrap(), full);
    }

    #[test]
    fn fsync_policies_also_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = SpoolWriter::for_session(tmp.path(), "s2", DurabilityPolicy::FsyncPerEvent);
        let event_id = EventId::new();
        let full = serde_json::json!({ "stdout": "data", "exit_code": 0 });
        let spool_ref = writer.write(&event_id, &full).unwrap();
        assert_eq!(read_spooled_output(tmp.path(), &spool_ref).unwrap(), full);
    }

    #[test]
    fn write_failure_is_a_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Occupy the session directory path with a regular FILE so the
        // spool directory cannot be created underneath it.
        std::fs::write(tmp.path().join("blocked"), b"not a directory").unwrap();
        let writer = SpoolWriter::for_session(tmp.path(), "blocked", DurabilityPolicy::Flush);

        let err = writer
            .write(&EventId::new(), &Value::String("payload".to_owned()))
            .expect_err("directory creation must fail");
        assert!(
            matches!(err, SessionPersistError::Io(_)),
            "expected a typed Io error, got {err:?}",
        );
    }

    #[test]
    fn traversal_and_malformed_refs_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in [
            "",
            "spool/x.bin",
            "a/b/c/d",
            "../spool/x.bin",
            "sess\\spool\\x.bin",
            "../../spool/x.bin",
            "sess/spool/../x.bin",
            "sess/spool/.hidden.bin",
            "sess/spool/x.json",
            "sess/spool/.bin",
            "sess/notspool/x.bin",
            "se ss/spool/x.bin",
            "sess/spool/sub/x.bin",
        ] {
            let err = resolve_spool_ref(tmp.path(), bad)
                .expect_err(&format!("ref {bad:?} must be rejected"));
            assert!(
                matches!(err, SessionPersistError::InvalidSpoolRef { .. }),
                "expected InvalidSpoolRef for {bad:?}, got {err:?}",
            );
        }
    }

    #[test]
    fn missing_spool_file_surfaces_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_spooled_output(tmp.path(), "sess/spool/gone.bin")
            .expect_err("missing file must error");
        assert!(matches!(err, SessionPersistError::Io(_)));
    }

    #[cfg(unix)]
    #[test]
    fn spool_directories_and_payloads_are_private_and_legacy_files_harden()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir()?;
        let data_dir = tmp.path().join("sessions");
        let writer = SpoolWriter::for_session(&data_dir, "sess", DurabilityPolicy::Flush);
        let event_id = EventId::new();
        let spool_ref = writer.write(&event_id, &serde_json::json!({ "secret": true }))?;
        let payload = resolve_spool_ref(&data_dir, &spool_ref)?;
        let mode = |path: &Path| -> std::io::Result<u32> {
            Ok(fs::metadata(path)?.permissions().mode() & 0o777)
        };

        assert_eq!(mode(&data_dir)?, 0o700);
        assert_eq!(mode(&data_dir.join("sess"))?, 0o700);
        assert_eq!(mode(&data_dir.join("sess/spool"))?, 0o700);
        assert_eq!(mode(&payload)?, 0o600);

        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(data_dir.join("sess"), fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(
            data_dir.join("sess/spool"),
            fs::Permissions::from_mode(0o755),
        )?;
        fs::set_permissions(&payload, fs::Permissions::from_mode(0o644))?;
        assert_eq!(
            read_spooled_output(&data_dir, &spool_ref)?,
            serde_json::json!({ "secret": true })
        );
        assert_eq!(mode(&data_dir)?, 0o700);
        assert_eq!(mode(&data_dir.join("sess"))?, 0o700);
        assert_eq!(mode(&data_dir.join("sess/spool"))?, 0o700);
        assert_eq!(mode(&payload)?, 0o600);

        let second_id = EventId::new();
        let second_ref = writer.write(&second_id, &serde_json::json!({ "secret": false }))?;
        assert_eq!(
            read_spooled_output(&data_dir, &second_ref)?,
            serde_json::json!({ "secret": false }),
        );
        assert_eq!(mode(&data_dir)?, 0o700);
        assert_eq!(mode(&data_dir.join("sess"))?, 0o700);
        assert_eq!(mode(&data_dir.join("sess/spool"))?, 0o700);
        assert_eq!(mode(&payload)?, 0o600);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn spool_reads_and_writes_refuse_symlinks_and_non_regular_files()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::fs;
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir()?;
        let spool_dir = tmp.path().join("sess/spool");
        fs::create_dir_all(&spool_dir)?;
        let target = tmp.path().join("outside.bin");
        fs::write(&target, br#"{"outside":true}"#)?;
        let linked = spool_dir.join("linked.bin");
        symlink(&target, &linked)?;

        let error = read_spooled_output(tmp.path(), "sess/spool/linked.bin")
            .err()
            .ok_or_else(|| std::io::Error::other("linked spool unexpectedly opened"))?;
        assert!(matches!(error, SessionPersistError::Io(_)));
        assert_eq!(fs::read(&target)?, br#"{"outside":true}"#);

        let writer = SpoolWriter::for_session(tmp.path(), "sess", DurabilityPolicy::Flush);
        let event_id = EventId::new();
        let occupied = spool_dir.join(format!("{event_id}.bin"));
        symlink(&target, &occupied)?;
        let error = writer
            .write(&event_id, &serde_json::json!({ "replacement": true }))
            .err()
            .ok_or_else(|| std::io::Error::other("linked spool was overwritten"))?;
        assert!(matches!(error, SessionPersistError::Io(_)));
        assert_eq!(fs::read(&target)?, br#"{"outside":true}"#);

        let directory = spool_dir.join("directory.bin");
        fs::create_dir(&directory)?;
        let error = read_spooled_output(tmp.path(), "sess/spool/directory.bin")
            .err()
            .ok_or_else(|| std::io::Error::other("spool directory unexpectedly opened"))?;
        assert!(matches!(error, SessionPersistError::Io(_)));
        Ok(())
    }
}
