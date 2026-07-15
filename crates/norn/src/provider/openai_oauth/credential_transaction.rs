//! Bounded credential transactions shared by cooperating Norn processes.

use std::collections::HashSet;
use std::fs::{File, TryLockError};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

use super::auth_root::NornAuthRoot;
use super::credential_decode::{MalformedCredentialReason, decode_auth_dot_json};
use super::storage::{AUTH_JSON_FILE, DeleteAuthOutcome, StorageError};
use super::types::AuthDotJson;
use crate::resource::DescriptorPermit;
use crate::util::PrivateRoot;

const CREDENTIAL_LOCK_FILE: &str = ".norn-auth.lock";
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(5);

static PROCESS_GATES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static PROCESS_GATE_CHANGED: Condvar = Condvar::new();

/// Raw-byte identity of one observed `auth.json` version.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CredentialRevision([u8; 32]);

impl std::fmt::Debug for CredentialRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialRevision([REDACTED])")
    }
}

/// Decoded state of one present credential document.
#[derive(Clone, Debug)]
pub(crate) enum CredentialDocument {
    /// No `auth.json` entry existed.
    Missing,
    /// The document decoded successfully.
    Parsed(Box<AuthDotJson>),
    /// Raw bytes existed but were not a usable credential document.
    Malformed(MalformedCredentialReason),
}

/// One credential document state and its exact raw revision.
#[derive(Clone, Debug)]
pub(crate) struct CredentialSnapshot {
    pub(crate) document: CredentialDocument,
    pub(crate) revision: Option<CredentialRevision>,
}

/// Failure to acquire or commit a credential transaction.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CredentialTransactionError {
    /// Descriptor admission rejected the private-filesystem operation.
    #[error("credential transaction could not acquire descriptor capacity: {0}")]
    DescriptorAdmission(#[source] Box<crate::resource::DescriptorAdmissionError>),
    /// The private credential root could not be opened safely.
    #[error("credential transaction could not open its private root: {0}")]
    OpenRoot(#[source] std::io::Error),
    /// The stable lock file could not be opened safely.
    #[error("credential transaction could not open its lock file: {0}")]
    OpenLock(#[source] std::io::Error),
    /// Another cooperating operation retained the lock past the caller's bound.
    #[error("credential transaction lock timed out after {waited:?}")]
    LockTimeout {
        /// Total caller-supplied acquisition budget.
        waited: Duration,
    },
    /// The operating-system lock operation failed.
    #[error("credential transaction lock failed: {0}")]
    Lock(#[source] std::io::Error),
    /// Credential serialization or filesystem I/O failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The credential changed after the caller's snapshot.
    #[error("credential changed during the operation; no replacement was written")]
    Conflict,
    /// Publication completed but read-back did not observe the proposed bytes.
    #[error("credential publication could not be verified because the file changed")]
    VerificationConflict,
    /// Proposed bytes were visible, but file or directory durability was not confirmed.
    #[error("credential bytes were visible but durability was not confirmed: {source}")]
    PublishedButUndurable {
        /// Revision of the credential that may already be visible.
        proposed_revision: CredentialRevision,
        /// Directory synchronization failure.
        #[source]
        source: std::io::Error,
    },
    /// Credential removal occurred, but directory durability was not confirmed.
    #[error("credential removal occurred but directory durability was not confirmed: {0}")]
    DeletedButUndurable(#[source] std::io::Error),
    /// A raced credential could not be safely and durably restored.
    #[error("a raced credential could not be safely and durably restored: {0}")]
    RecoveryIncomplete(#[source] std::io::Error),
}

#[derive(Debug)]
struct ProcessGuard {
    identity: PathBuf,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let mut active = match PROCESS_GATES.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.remove(&self.identity);
        PROCESS_GATE_CHANGED.notify_all();
    }
}

impl ProcessGuard {
    fn is_held(&self) -> bool {
        !self.identity.as_os_str().is_empty()
    }
}

/// Exclusive transaction over one file-backed credential identity.
pub(crate) struct CredentialTransaction {
    file: File,
    root: PrivateRoot,
    process_guard: ProcessGuard,
    descriptor_permit: DescriptorPermit,
}

impl std::fmt::Debug for CredentialTransaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialTransaction")
            .field("process_gate_held", &self.process_guard.is_held())
            .field("descriptor_weight", &self.descriptor_permit.weight())
            .finish_non_exhaustive()
    }
}

impl Drop for CredentialTransaction {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!(%error, "failed to explicitly unlock OAuth credential transaction");
        }
    }
}

impl CredentialTransaction {
    /// Inspect the current raw revision without acquiring the writer lock.
    /// This is suitable for capturing an optimistic revision before a
    /// long-running browser flow. The eventual writer must still acquire a
    /// transaction and compare this revision before publication.
    pub(crate) fn inspect(
        auth_root: &NornAuthRoot,
    ) -> Result<CredentialSnapshot, CredentialTransactionError> {
        let descriptor_permit = crate::resource::acquire_private_fs()
            .map_err(Box::new)
            .map_err(CredentialTransactionError::DescriptorAdmission)?;
        let root = match PrivateRoot::open(auth_root.as_path()) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                drop(descriptor_permit);
                return Ok(CredentialSnapshot {
                    document: CredentialDocument::Missing,
                    revision: None,
                });
            }
            Err(error) => return Err(CredentialTransactionError::OpenRoot(error)),
        };
        let snapshot = snapshot_from_root(&root);
        drop(root);
        drop(descriptor_permit);
        snapshot
    }

    /// Acquire the process-local gate and stable inter-process lock.
    pub(crate) fn acquire(
        auth_root: &NornAuthRoot,
        deadline: Duration,
    ) -> Result<Self, CredentialTransactionError> {
        let started = Instant::now();
        let identity = auth_root.as_path().join(CREDENTIAL_LOCK_FILE);
        let process_guard = lock_process_gate(&identity, deadline, started)?;
        let descriptor_permit = crate::resource::acquire_private_fs()
            .map_err(Box::new)
            .map_err(CredentialTransactionError::DescriptorAdmission)?;
        let root = PrivateRoot::create_with_durable_ancestors(auth_root.as_path())
            .map_err(CredentialTransactionError::OpenRoot)?;
        let file = root
            .open_lock(Path::new(CREDENTIAL_LOCK_FILE))
            .map_err(CredentialTransactionError::OpenLock)?;
        lock_file(&file, deadline, started)?;
        Ok(Self {
            file,
            root,
            process_guard,
            descriptor_permit,
        })
    }

    /// Read and decode the current document while retaining its raw revision.
    pub(crate) fn snapshot(&self) -> Result<CredentialSnapshot, CredentialTransactionError> {
        snapshot_from_root(&self.root)
    }

    /// Save `auth` when the caller's raw revision is still current.
    /// The stable lock serializes cooperating Norn writers. A lock-ignoring
    /// writer observed before publication produces [`CredentialTransactionError::Conflict`].
    /// Portable rename does not provide content compare-and-swap, so no claim is
    /// made about a foreign replacement in the final compare-to-rename window.
    pub(crate) fn save_if_revision(
        &self,
        expected: Option<&CredentialRevision>,
        auth: &AuthDotJson,
    ) -> Result<CredentialRevision, CredentialTransactionError> {
        let mut proposed = serde_json::to_vec_pretty(auth).map_err(StorageError::Json)?;
        proposed.push(b'\n');
        let proposed_revision = revision(&proposed);
        let current = self.current_revision()?;
        if current.as_ref() == Some(&proposed_revision) {
            self.sync_existing_revision(&proposed_revision)?;
            return Ok(proposed_revision);
        }
        if current.as_ref() != expected {
            return Err(CredentialTransactionError::Conflict);
        }

        let temporary = temporary_path();
        let write_result = (|| -> Result<(), CredentialTransactionError> {
            let mut file = self.root.create_new(&temporary).map_err(StorageError::Io)?;
            file.write_all(&proposed).map_err(StorageError::Io)?;
            file.sync_all().map_err(StorageError::Io)?;
            drop(file);
            self.root
                .rename(&temporary, Path::new(AUTH_JSON_FILE))
                .map_err(StorageError::Io)?;
            self.root.sync_dir(Path::new("")).map_err(|source| {
                CredentialTransactionError::PublishedButUndurable {
                    proposed_revision: proposed_revision.clone(),
                    source,
                }
            })?;
            Ok(())
        })();
        if write_result.is_err() {
            self.cleanup_temporary(&temporary);
        }
        write_result?;

        if self.current_revision()?.as_ref() != Some(&proposed_revision) {
            return Err(CredentialTransactionError::VerificationConflict);
        }
        Ok(proposed_revision)
    }

    /// Durably delete exactly the credential named by `expected`.
    /// The canonical entry is first moved to a unique private quarantine name
    /// and verified there. A raced replacement is restored without replacing a
    /// newer canonical entry; it is never unlinked as the caller's credential.
    pub(crate) fn delete_if_revision(
        &self,
        expected: Option<&CredentialRevision>,
    ) -> Result<DeleteAuthOutcome, CredentialTransactionError> {
        let quarantine = quarantine_path();
        self.delete_if_revision_at(expected, &quarantine)
    }

    fn delete_if_revision_at(
        &self,
        expected: Option<&CredentialRevision>,
        quarantine: &Path,
    ) -> Result<DeleteAuthOutcome, CredentialTransactionError> {
        match self.root.rename_new(Path::new(AUTH_JSON_FILE), quarantine) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && expected.is_none() => {
                return Ok(DeleteAuthOutcome::Absent);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CredentialTransactionError::Conflict);
            }
            Err(error) => return Err(StorageError::Io(error).into()),
        }
        let moved_revision = match self.read_raw_at(quarantine) {
            Ok(raw) => revision(&raw),
            Err(error) => {
                self.restore_quarantine(quarantine)?;
                return Err(error);
            }
        };
        if Some(&moved_revision) != expected {
            self.restore_quarantine(quarantine)?;
            return Err(CredentialTransactionError::Conflict);
        }
        if let Err(error) = self.root.remove_file(quarantine) {
            self.restore_quarantine(quarantine)?;
            return Err(StorageError::Io(error).into());
        }
        self.root
            .sync_dir(Path::new(""))
            .map_err(CredentialTransactionError::DeletedButUndurable)?;
        Ok(DeleteAuthOutcome::Removed)
    }

    fn current_revision(&self) -> Result<Option<CredentialRevision>, CredentialTransactionError> {
        self.read_raw().map(|raw| raw.as_deref().map(revision))
    }

    fn sync_existing_revision(
        &self,
        proposed_revision: &CredentialRevision,
    ) -> Result<(), CredentialTransactionError> {
        let mut file = self
            .root
            .open_read_append(Path::new(AUTH_JSON_FILE))
            .map_err(|error| match error.kind() {
                ErrorKind::NotFound => CredentialTransactionError::VerificationConflict,
                _ => CredentialTransactionError::Storage(StorageError::Io(error)),
            })?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw).map_err(StorageError::Io)?;
        if &revision(&raw) != proposed_revision {
            return Err(CredentialTransactionError::VerificationConflict);
        }
        file.sync_all()
            .map_err(|source| CredentialTransactionError::PublishedButUndurable {
                proposed_revision: proposed_revision.clone(),
                source,
            })?;
        self.root.sync_dir(Path::new("")).map_err(|source| {
            CredentialTransactionError::PublishedButUndurable {
                proposed_revision: proposed_revision.clone(),
                source,
            }
        })?;
        if self.current_revision()?.as_ref() != Some(proposed_revision) {
            return Err(CredentialTransactionError::VerificationConflict);
        }
        Ok(())
    }

    fn read_raw(&self) -> Result<Option<Vec<u8>>, CredentialTransactionError> {
        let mut file = match self.root.open_read(Path::new(AUTH_JSON_FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error).into()),
        };
        let mut raw = Vec::new();
        file.read_to_end(&mut raw).map_err(StorageError::Io)?;
        Ok(Some(raw))
    }

    fn read_raw_at(&self, relative: &Path) -> Result<Vec<u8>, CredentialTransactionError> {
        let mut file = self.root.open_read(relative).map_err(StorageError::Io)?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw).map_err(StorageError::Io)?;
        Ok(raw)
    }

    fn restore_quarantine(&self, quarantine: &Path) -> Result<(), CredentialTransactionError> {
        match self.root.rename_new(quarantine, Path::new(AUTH_JSON_FILE)) {
            Ok(()) => self
                .root
                .sync_dir(Path::new(""))
                .map_err(CredentialTransactionError::RecoveryIncomplete),
            Err(error) => Err(CredentialTransactionError::RecoveryIncomplete(error)),
        }
    }

    fn cleanup_temporary(&self, temporary: &Path) {
        match self.root.remove_file(temporary) {
            Ok(()) => {
                if let Err(error) = self.root.sync_dir(Path::new("")) {
                    tracing::warn!(
                        %error,
                        "OAuth credential temporary cleanup was not durably confirmed"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, "failed to clean up OAuth credential temporary file");
            }
        }
    }
}

fn snapshot_from_root(
    root: &PrivateRoot,
) -> Result<CredentialSnapshot, CredentialTransactionError> {
    let mut file = match root.open_read(Path::new(AUTH_JSON_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CredentialSnapshot {
                document: CredentialDocument::Missing,
                revision: None,
            });
        }
        Err(error) => return Err(StorageError::Io(error).into()),
    };
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(StorageError::Io)?;
    let revision = Some(revision(&raw));
    let document = match decode_auth_dot_json(&raw) {
        Ok(auth) => CredentialDocument::Parsed(Box::new(auth)),
        Err(reason) => {
            tracing::debug!(
                ?reason,
                "credential transaction observed malformed credentials"
            );
            CredentialDocument::Malformed(reason)
        }
    };
    Ok(CredentialSnapshot { document, revision })
}

fn revision(raw: &[u8]) -> CredentialRevision {
    CredentialRevision(Sha256::digest(raw).into())
}

fn temporary_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "{AUTH_JSON_FILE}.{}.{sequence}.transaction.tmp",
        std::process::id()
    ))
}

fn quarantine_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "{AUTH_JSON_FILE}.{}.{sequence}.logout-quarantine",
        std::process::id()
    ))
}

fn lock_process_gate(
    identity: &Path,
    deadline: Duration,
    started: Instant,
) -> Result<ProcessGuard, CredentialTransactionError> {
    let mut active = match PROCESS_GATES.lock() {
        Ok(active) => active,
        Err(poisoned) => poisoned.into_inner(),
    };
    loop {
        if started.elapsed() >= deadline {
            return Err(CredentialTransactionError::LockTimeout { waited: deadline });
        }
        if !active.contains(identity) {
            active.insert(identity.to_path_buf());
            return Ok(ProcessGuard {
                identity: identity.to_path_buf(),
            });
        }
        let Some(remaining) = deadline.checked_sub(started.elapsed()) else {
            return Err(CredentialTransactionError::LockTimeout { waited: deadline });
        };
        let (next, timeout) = match PROCESS_GATE_CHANGED.wait_timeout(active, remaining) {
            Ok(result) => result,
            Err(poisoned) => poisoned.into_inner(),
        };
        active = next;
        if timeout.timed_out() && active.contains(identity) {
            return Err(CredentialTransactionError::LockTimeout { waited: deadline });
        }
    }
}

fn lock_file(
    file: &File,
    deadline: Duration,
    started: Instant,
) -> Result<(), CredentialTransactionError> {
    loop {
        if started.elapsed() >= deadline {
            return Err(CredentialTransactionError::LockTimeout { waited: deadline });
        }
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => {
                let Some(remaining) = deadline.checked_sub(started.elapsed()) else {
                    return Err(CredentialTransactionError::LockTimeout { waited: deadline });
                };
                std::thread::sleep(remaining.min(LOCK_POLL_INTERVAL));
            }
            Err(TryLockError::Error(error)) => {
                return Err(CredentialTransactionError::Lock(error));
            }
        }
    }
}

#[cfg(test)]
#[path = "credential_transaction_tests.rs"]
mod tests;
