//! Mutation ledger: a derived view over the session
//! [`ActionLog`](crate::session::action_log::ActionLog) tracking every file
//! the agent changed during the session.
//!
//! The ledger answers "what files did I change?" without consulting git
//! status — git would surface pre-existing dirty files and concurrent edits
//! from other agents sharing the working tree. Instead, the ledger is built
//! purely from the agent's own completed mutation-tool calls (`edit`, `write`,
//! `apply_patch`): each successful completion calls
//! [`MutationLedger::record_mutation`], which merges into a per-file entry and
//! captures the file's post-mutation content hash as the revert baseline.
//!
//! **External revert detection is lazy.** No filesystem watcher, no polling.
//! When the ledger is queried, each tracked file is read and hashed *at that
//! moment* and compared against the post-mutation baseline recorded when the
//! file's most recent tool call completed:
//!
//! * hash differs from the baseline (the file was changed, deleted, or
//!   recreated after the agent's last tool call left it) →
//!   [`RevertStatus::ExternallyReverted`]
//! * hash matches the baseline and no later tool call ever changed the file
//!   relative to an earlier recorded baseline → [`RevertStatus::Active`]
//! * hash matches the baseline but a later tool call had changed the file
//!   relative to an earlier recorded baseline →
//!   [`RevertStatus::SubsequentlyModified`]
//!
//! Whether a later tool call superseded an earlier baseline is decided by
//! comparing post-mutation hashes as mutations are recorded — not by counting
//! how many tool calls touched the file — so a no-op re-edit that leaves the
//! content unchanged does not flip a file to `SubsequentlyModified`.
//!
//! Deletions recorded by the agent store an absent-file baseline; a
//! still-deleted file therefore reads back as [`RevertStatus::Active`] (the
//! deletion is intact), while a file that reappeared reads back as
//! [`RevertStatus::ExternallyReverted`].
//!
//! **Missing is not the same as unreadable.** A file that cannot be read
//! for any reason other than not existing (permission change, path
//! component turned into a file, transient I/O failure) proves nothing
//! about its content, so it is never treated as deleted: the affected
//! entry reads back as [`RevertStatus::Unknown`] instead of miscalibrating
//! the revert baseline, and the read failure is logged.
//!
//! The ledger is session-scoped: it lives inside a single
//! [`ActionLog`](crate::session::action_log::ActionLog) instance and only ever
//! sees that instance's `record_completion` calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Net line-count change a mutation applied to a file.
///
/// Accumulated across every mutation the agent makes to the same file during
/// the session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    /// Total lines added to the file across recorded mutations.
    pub lines_added: u32,
    /// Total lines removed from the file across recorded mutations.
    pub lines_removed: u32,
}

/// The kind of change a mutation tool applied to a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationOp {
    /// The file did not exist before the mutation and was created.
    Created,
    /// The file existed before the mutation and its contents were changed.
    Modified,
    /// The file was removed.
    Deleted,
}

/// Whether a recorded mutation's effect is still present on disk at query time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevertStatus {
    /// The file still matches the content the agent's last tool call left,
    /// and no later tool call touched it. The change is intact.
    Active,
    /// The file's content no longer matches the recorded post-mutation
    /// baseline and no agent tool call touched it after the last recorded
    /// mutation — something outside the agent reverted or changed it
    /// (including external deletion).
    ExternallyReverted,
    /// The file still matches the agent's most recent tool call, but a later
    /// tool call had changed the file's content relative to an earlier
    /// recorded baseline — the original recorded effect was superseded by a
    /// subsequent agent mutation.
    SubsequentlyModified,
    /// The comparison could not be made: the file (or its recorded
    /// baseline) was unreadable for a reason other than not existing —
    /// e.g. a permission change — so no claim about the mutation's
    /// intactness would be evidence-based. The read failure is logged
    /// when it is observed.
    Unknown,
}

/// A single file's merged mutation history, surfaced by the `mutations` query.
///
/// One entry exists per file the agent changed during the session. Repeated
/// mutations to the same file merge into this entry: `first_tool_call_id` pins
/// the first touch, `last_tool_call_id` tracks the most recent, and
/// `diff_stats` accumulates. `revert_status` is computed lazily at query time
/// from the filesystem — it is never stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MutationLedgerEntry {
    /// Path of the file the agent changed.
    pub file_path: PathBuf,
    /// The most recent operation applied to the file.
    pub operation: MutationOp,
    /// Tool-call id of the first agent tool call that touched this file.
    pub first_tool_call_id: String,
    /// Tool-call id of the most recent agent tool call that touched this file.
    pub last_tool_call_id: String,
    /// Whether the recorded effect is still present on disk, evaluated at the
    /// time the entry was produced.
    pub revert_status: RevertStatus,
    /// Accumulated net line-count change across every recorded mutation to
    /// this file.
    pub diff_stats: DiffStats,
}

/// One mutation extracted from a completed mutation-tool call, ready to be
/// merged into the ledger via [`MutationLedger::record_mutation`].
#[derive(Clone, Debug)]
pub struct RecordedMutation {
    /// Path of the file the tool changed.
    pub file_path: PathBuf,
    /// Operation the tool applied.
    pub operation: MutationOp,
    /// Tool-call id of the completing tool call.
    pub tool_call_id: String,
    /// Line-count change this single mutation applied.
    pub diff_stats: DiffStats,
}

/// Typed result of fingerprinting a file's on-disk state, distinguishing
/// "provably absent" from "could not be inspected".
#[derive(Clone, Debug, PartialEq, Eq)]
enum Fingerprint {
    /// The file exists and hashed to this lowercase-hex SHA-256 digest.
    Content(String),
    /// The file provably does not exist (`ErrorKind::NotFound`) — the
    /// baseline recorded for an agent deletion, and the state a
    /// still-deleted file reads back as.
    Missing,
    /// The file could not be read for a reason other than absence (e.g.
    /// permission denied). Two `Unreadable` fingerprints prove nothing
    /// about content equality, so this state never participates in
    /// baseline comparisons — it forces [`RevertStatus::Unknown`].
    Unreadable,
}

/// Internal per-file state. Carries the revert baseline (`baseline`) and
/// the `superseded` flag, which the public [`MutationLedgerEntry`] does not
/// expose, and from which `revert_status` is derived at query time.
#[derive(Clone, Debug)]
struct LedgerRecord {
    file_path: PathBuf,
    operation: MutationOp,
    first_tool_call_id: String,
    last_tool_call_id: String,
    diff_stats: DiffStats,
    /// Post-mutation fingerprint captured when the most recent tool call
    /// touching this file completed ([`Fingerprint::Missing`] when that
    /// call deleted the file).
    baseline: Fingerprint,
    /// `true` once a later tool call recorded a post-mutation fingerprint
    /// that differed from the baseline an earlier call left — i.e. a
    /// subsequent agent mutation changed this file's content. Decided by
    /// comparing fingerprints at record time, never by counting touches;
    /// an [`Fingerprint::Unreadable`] observation on either side leaves
    /// the flag untouched because it proves nothing.
    superseded: bool,
}

impl LedgerRecord {
    /// Resolve the public entry, computing `revert_status` against the
    /// current filesystem state.
    fn into_entry(self) -> MutationLedgerEntry {
        let revert_status = self.compute_revert_status();
        MutationLedgerEntry {
            file_path: self.file_path,
            operation: self.operation,
            first_tool_call_id: self.first_tool_call_id,
            last_tool_call_id: self.last_tool_call_id,
            revert_status,
            diff_stats: self.diff_stats,
        }
    }

    /// Lazily classify the file's current state relative to the recorded
    /// baseline.
    ///
    /// A current fingerprint that differs from the baseline can only have
    /// arisen after the agent's most recent tool call, so it is an external
    /// change (covering external deletion and recreation). A current
    /// fingerprint that matches the baseline means the file is exactly as
    /// the last tool call left it: it is
    /// [`RevertStatus::SubsequentlyModified`] when a later tool call had
    /// changed the file relative to an earlier recorded baseline, and
    /// [`RevertStatus::Active`] otherwise. When either side is
    /// [`Fingerprint::Unreadable`] no evidence-based comparison exists and
    /// the status is [`RevertStatus::Unknown`].
    fn compute_revert_status(&self) -> RevertStatus {
        let current = fingerprint(&self.file_path);
        match (&self.baseline, &current) {
            (Fingerprint::Unreadable, _) | (_, Fingerprint::Unreadable) => RevertStatus::Unknown,
            (baseline, current) if baseline == current => {
                if self.superseded {
                    RevertStatus::SubsequentlyModified
                } else {
                    RevertStatus::Active
                }
            }
            _ => RevertStatus::ExternallyReverted,
        }
    }
}

/// Session-scoped, in-memory ledger of the agent's file mutations.
///
/// Owned by a single [`ActionLog`](crate::session::action_log::ActionLog).
/// Thread-safe via [`parking_lot::RwLock`], mirroring the action log's own
/// concurrency model. Query-time hashing happens outside the lock.
#[derive(Debug, Default)]
pub struct MutationLedger {
    inner: RwLock<HashMap<PathBuf, LedgerRecord>>,
}

impl MutationLedger {
    /// Create an empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct files the ledger is tracking.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Whether the ledger is tracking any files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Merge a single mutation into the ledger.
    ///
    /// Creates a new per-file entry on first touch, or updates the existing
    /// one: `first_tool_call_id` is preserved, `last_tool_call_id` advances,
    /// `diff_stats` accumulate, and the revert baseline is recomputed from
    /// the file's post-mutation content (`Fingerprint::Missing` for
    /// deletions). When that new fingerprint differs from the baseline an
    /// earlier call left — and both sides are readable, so the comparison
    /// is evidence-based — the entry is flagged as superseded by a later
    /// tool call. The fingerprint is computed before the lock is taken so
    /// disk I/O never blocks readers.
    pub fn record_mutation(&self, mutation: RecordedMutation) {
        let RecordedMutation {
            file_path,
            operation,
            tool_call_id,
            diff_stats,
        } = mutation;

        let baseline = match operation {
            MutationOp::Deleted => Fingerprint::Missing,
            MutationOp::Created | MutationOp::Modified => fingerprint(&file_path),
        };

        let mut map = self.inner.write();
        if let Some(record) = map.get_mut(&file_path) {
            // A later tool call whose post-mutation content differs from the
            // baseline an earlier call left has superseded that baseline.
            // Unreadable fingerprints prove nothing, so they never flip the
            // flag.
            if baseline != Fingerprint::Unreadable
                && record.baseline != Fingerprint::Unreadable
                && baseline != record.baseline
            {
                record.superseded = true;
            }
            record.operation = operation;
            record.last_tool_call_id = tool_call_id;
            record.diff_stats.lines_added = record
                .diff_stats
                .lines_added
                .saturating_add(diff_stats.lines_added);
            record.diff_stats.lines_removed = record
                .diff_stats
                .lines_removed
                .saturating_add(diff_stats.lines_removed);
            record.baseline = baseline;
        } else {
            let record = LedgerRecord {
                file_path: file_path.clone(),
                operation,
                first_tool_call_id: tool_call_id.clone(),
                last_tool_call_id: tool_call_id,
                diff_stats,
                baseline,
                superseded: false,
            };
            map.insert(file_path, record);
        }
    }

    /// Return every tracked file's entry with `revert_status` evaluated
    /// against the current filesystem state.
    ///
    /// Records are cloned out under the read lock, then hashed without holding
    /// it.
    #[must_use]
    pub fn entries(&self) -> Vec<MutationLedgerEntry> {
        let records: Vec<LedgerRecord> = self.inner.read().values().cloned().collect();
        records.into_iter().map(LedgerRecord::into_entry).collect()
    }

    /// Return one file's entry with `revert_status` evaluated against the
    /// current filesystem state, or `None` when the file is not tracked.
    #[must_use]
    pub fn entry(&self, file_path: &Path) -> Option<MutationLedgerEntry> {
        let record = self.inner.read().get(file_path).cloned();
        record.map(LedgerRecord::into_entry)
    }
}

/// Fingerprint the file at `path`: SHA-256 of its content, a typed
/// missing marker, or a typed unreadable marker.
///
/// Only `ErrorKind::NotFound` proves absence ([`Fingerprint::Missing`]):
/// an externally deleted file then compares unequal to a real content
/// baseline (→ externally reverted) while matching the baseline stored
/// for a recorded deletion (→ deletion intact). Any other read failure
/// (permission change, transient I/O error) proves nothing about the
/// content, so it is logged and reported as [`Fingerprint::Unreadable`]
/// — never silently conflated with deletion, which would miscalibrate
/// the revert baseline.
fn fingerprint(path: &Path) -> Fingerprint {
    let _permit = match crate::session::persistence::acquire_private_fs() {
        Ok(permit) => permit,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "mutation ledger could not admit a tracked-file read; its revert status is Unknown",
            );
            return Fingerprint::Unreadable;
        }
    };
    match std::fs::read(path) {
        Ok(bytes) => Fingerprint::Content(hash_bytes(&bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Fingerprint::Missing,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "mutation ledger could not read a tracked file for a \
                 reason other than absence; its revert status is Unknown \
                 until the file is readable again",
            );
            Fingerprint::Unreadable
        }
    }
}

/// Lowercase hex SHA-256 of `bytes`.
fn hash_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Writing a byte to a String is infallible.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::similar_names,
    clippy::uninlined_format_args,
    clippy::doc_markdown
)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn mutation(
        path: &Path,
        op: MutationOp,
        id: &str,
        added: u32,
        removed: u32,
    ) -> RecordedMutation {
        RecordedMutation {
            file_path: path.to_path_buf(),
            operation: op,
            tool_call_id: id.to_owned(),
            diff_stats: DiffStats {
                lines_added: added,
                lines_removed: removed,
            },
        }
    }

    #[test]
    fn mutation_op_has_exactly_three_variants() {
        for (op, name) in [
            (MutationOp::Created, "\"Created\""),
            (MutationOp::Modified, "\"Modified\""),
            (MutationOp::Deleted, "\"Deleted\""),
        ] {
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(json, name);
            let parsed: MutationOp = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, op);
            // Exhaustive match guards against silently adding a variant.
            match op {
                MutationOp::Created | MutationOp::Modified | MutationOp::Deleted => {}
            }
        }
    }

    #[test]
    fn revert_status_has_exactly_four_variants() {
        for (status, name) in [
            (RevertStatus::Active, "\"Active\""),
            (RevertStatus::ExternallyReverted, "\"ExternallyReverted\""),
            (
                RevertStatus::SubsequentlyModified,
                "\"SubsequentlyModified\"",
            ),
            (RevertStatus::Unknown, "\"Unknown\""),
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, name);
            let parsed: RevertStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
            match status {
                RevertStatus::Active
                | RevertStatus::ExternallyReverted
                | RevertStatus::SubsequentlyModified
                | RevertStatus::Unknown => {}
            }
        }
    }

    #[test]
    fn entry_serde_roundtrip_preserves_all_fields() {
        let entry = MutationLedgerEntry {
            file_path: PathBuf::from("src/a.rs"),
            operation: MutationOp::Modified,
            first_tool_call_id: "tc-1".to_owned(),
            last_tool_call_id: "tc-2".to_owned(),
            revert_status: RevertStatus::Active,
            diff_stats: DiffStats {
                lines_added: 5,
                lines_removed: 2,
            },
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MutationLedgerEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.file_path, PathBuf::from("src/a.rs"));
        assert_eq!(parsed.operation, MutationOp::Modified);
        assert_eq!(parsed.first_tool_call_id, "tc-1");
        assert_eq!(parsed.last_tool_call_id, "tc-2");
        assert_eq!(parsed.revert_status, RevertStatus::Active);
        assert_eq!(parsed.diff_stats.lines_added, 5);
        assert_eq!(parsed.diff_stats.lines_removed, 2);
    }

    #[test]
    fn single_mutation_creates_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "fn main() {}\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Created, "tc-1", 1, 0));

        let entries = ledger.entries();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.file_path, path);
        assert_eq!(entry.operation, MutationOp::Created);
        assert_eq!(entry.first_tool_call_id, "tc-1");
        assert_eq!(entry.last_tool_call_id, "tc-1");
        assert_eq!(entry.diff_stats.lines_added, 1);
        assert_eq!(entry.diff_stats.lines_removed, 0);
        // File unchanged since the single mutation.
        assert_eq!(entry.revert_status, RevertStatus::Active);
    }

    #[test]
    fn two_mutations_same_file_merge() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "one\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Created, "tc-1", 1, 0));
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-2", 2, 1));

        let entry = ledger.entry(&path).unwrap();
        assert_eq!(entry.first_tool_call_id, "tc-1", "first touch is preserved");
        assert_eq!(entry.last_tool_call_id, "tc-2", "last touch advances");
        assert_eq!(entry.operation, MutationOp::Modified);
        assert_eq!(entry.diff_stats.lines_added, 3, "diff stats accumulate");
        assert_eq!(entry.diff_stats.lines_removed, 1);
    }

    #[test]
    fn unchanged_file_is_active() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "stable\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-1", 1, 0));

        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::Active
        );
    }

    #[test]
    fn external_change_is_externally_reverted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "original\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-1", 1, 0));

        // Something outside the agent rewrites the file — no record_mutation.
        fs::write(&path, "tampered\n").unwrap();

        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::ExternallyReverted
        );
    }

    #[test]
    fn subsequent_tool_mutation_is_subsequently_modified() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "v1\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-1", 1, 0));

        // An external change lands, then a second agent tool call re-touches
        // the file and records its mutation.
        fs::write(&path, "external\n").unwrap();
        fs::write(&path, "v3\n").unwrap();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-2", 1, 1));

        let entry = ledger.entry(&path).unwrap();
        assert_eq!(entry.revert_status, RevertStatus::SubsequentlyModified);
        assert_eq!(
            entry.last_tool_call_id, "tc-2",
            "latest entry queryable with the second tool_call_id"
        );
    }

    #[test]
    fn repeated_mutation_with_unchanged_content_stays_active() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "same\n").unwrap();

        let ledger = MutationLedger::new();
        // Two tool calls touch the file but neither changes its content, so the
        // baseline never moves. Classification is by hash comparison, not by
        // touch count, so the file stays Active rather than being reported as
        // SubsequentlyModified.
        ledger.record_mutation(mutation(&path, MutationOp::Created, "tc-1", 1, 0));
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-2", 0, 0));

        let entry = ledger.entry(&path).unwrap();
        assert_eq!(entry.last_tool_call_id, "tc-2");
        assert_eq!(
            entry.revert_status,
            RevertStatus::Active,
            "a no-op second edit must not be reported as SubsequentlyModified"
        );
    }

    #[test]
    fn deleted_file_after_modify_is_externally_reverted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "present\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-1", 1, 0));

        fs::remove_file(&path).unwrap();

        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::ExternallyReverted
        );
    }

    #[test]
    fn recorded_deletion_still_deleted_is_active() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "to be removed\n").unwrap();
        fs::remove_file(&path).unwrap();

        let ledger = MutationLedger::new();
        // The agent's apply_patch deleted the file; record it as Deleted.
        ledger.record_mutation(mutation(&path, MutationOp::Deleted, "tc-1", 0, 3));

        let entry = ledger.entry(&path).unwrap();
        assert_eq!(entry.operation, MutationOp::Deleted);
        assert_eq!(
            entry.revert_status,
            RevertStatus::Active,
            "a still-deleted file matches the deletion sentinel"
        );
    }

    #[test]
    fn recorded_deletion_then_recreated_is_externally_reverted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "content\n").unwrap();
        fs::remove_file(&path).unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Deleted, "tc-1", 0, 1));

        // The file reappears outside the agent's control.
        fs::write(&path, "resurrected\n").unwrap();

        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::ExternallyReverted
        );
    }

    /// Make `path` unreadable for the current user. Returns `false` (and
    /// logs a skip) when the environment cannot produce an unreadable
    /// file — e.g. running as root, where mode 000 still reads fine.
    #[cfg(unix)]
    fn make_unreadable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(path).is_ok() {
            tracing::info!(
                path = %path.display(),
                "skipping unreadable-file assertion: this environment \
                 (likely root) can read mode-000 files",
            );
            return false;
        }
        true
    }

    /// Undo [`make_unreadable`].
    #[cfg(unix)]
    fn restore_readable(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// Regression: an EACCES-style read failure at QUERY time used to
    /// collapse to the deleted sentinel, reporting a content-baselined
    /// file as ExternallyReverted (and a recorded deletion as Active)
    /// with zero evidence. Unreadable must surface as Unknown.
    #[cfg(unix)]
    #[test]
    fn unreadable_file_at_query_time_is_unknown_not_reverted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "content\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-1", 1, 0));

        if !make_unreadable(&path) {
            return;
        }
        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::Unknown,
            "an unreadable file proves nothing; it must not be reported \
             as reverted or intact",
        );

        restore_readable(&path);
        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::Active,
            "readability restored with unchanged content is Active again",
        );
    }

    /// Regression: with the sentinel scheme, a recorded deletion whose
    /// path later becomes unreadable-but-present read back as Active
    /// ("deletion intact") even though the file demonstrably exists.
    #[cfg(unix)]
    #[test]
    fn unreadable_resurrected_file_after_deletion_is_unknown_not_active() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "doomed\n").unwrap();
        fs::remove_file(&path).unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Deleted, "tc-1", 0, 1));

        // The file reappears but cannot be read: its content is unknown,
        // so the deletion must not be reported as intact.
        fs::write(&path, "resurrected\n").unwrap();
        if !make_unreadable(&path) {
            return;
        }
        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::Unknown,
        );
    }

    /// Regression: an unreadable file at RECORD time used to store the
    /// deleted sentinel as the revert baseline, so a matching unreadable
    /// read at query time reported Active. The baseline must be typed
    /// Unreadable and the status Unknown.
    #[cfg(unix)]
    #[test]
    fn unreadable_file_at_record_time_yields_unknown_baseline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "written then locked down\n").unwrap();
        if !make_unreadable(&path) {
            return;
        }

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-1", 1, 0));

        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::Unknown,
            "a baseline that was never readable can support no claim",
        );

        // Even after the file becomes readable, the baseline itself is
        // unreadable — no evidence-based comparison exists until a later
        // mutation records a readable baseline.
        restore_readable(&path);
        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::Unknown,
        );
        ledger.record_mutation(mutation(&path, MutationOp::Modified, "tc-2", 0, 0));
        assert_eq!(
            ledger.entry(&path).unwrap().revert_status,
            RevertStatus::Active,
            "a later readable baseline restores evidence-based status",
        );
    }

    #[test]
    fn entry_retrievable_by_path_and_unknown_is_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "x\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&path, MutationOp::Created, "tc-1", 1, 0));

        assert!(ledger.entry(&path).is_some());
        assert!(ledger.entry(&dir.path().join("missing.rs")).is_none());
    }

    #[test]
    fn distinct_files_tracked_independently() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        fs::write(&a, "a\n").unwrap();
        fs::write(&b, "b\n").unwrap();

        let ledger = MutationLedger::new();
        ledger.record_mutation(mutation(&a, MutationOp::Created, "tc-1", 1, 0));
        ledger.record_mutation(mutation(&b, MutationOp::Created, "tc-2", 1, 0));

        assert_eq!(ledger.len(), 2);
        assert!(!ledger.is_empty());
        assert_eq!(ledger.entry(&a).unwrap().first_tool_call_id, "tc-1");
        assert_eq!(ledger.entry(&b).unwrap().first_tool_call_id, "tc-2");
    }
}
