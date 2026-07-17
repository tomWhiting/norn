//! Per-timeline transactions for strict session append and recovery.

#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::ops::{Deref, DerefMut};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use parking_lot::{Condvar, Mutex};
use sha2::{Digest as _, Sha256};

#[cfg(test)]
use super::acquire_private_fs;
use super::lock::IndexLock;
use super::types::SessionPersistError;
#[cfg(test)]
use crate::resource::DescriptorPermit;
use crate::util::PrivateRoot;

static PROCESS_TIMELINE_GATES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static PROCESS_TIMELINE_GATE_CHANGED: Condvar = Condvar::new();
#[cfg(test)]
static PROCESS_TIMELINE_WAITERS: LazyLock<Mutex<HashMap<PathBuf, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
#[cfg(test)]
static PROCESS_TIMELINE_WAITER_CHANGED: Condvar = Condvar::new();

const TIMELINE_LOCK_DIRECTORY: &str = ".timeline-locks";
const TIMELINE_LOCK_DOMAIN: &[u8] = b"norn-session-timeline-lock-v1\0";

/// One descriptor-admitted, inter-process transaction over a timeline path.
///
/// Acquisition has no implicit timeout. The process-local gate is taken before
/// any descriptor is opened, then a retained lock in `.timeline-locks/` is held
/// for the entire recovery/classification/write operation. Keeping the lock
/// outside deletable artifacts prevents unlinking from splitting lock owners.
#[cfg(test)]
#[derive(Debug)]
pub(super) struct TimelineTransaction {
    _lock: TimelineLockGuard,
    root: PrivateRoot,
    _descriptor_permit: DescriptorPermit,
}

#[cfg(test)]
impl TimelineTransaction {
    /// Acquire a transaction that may create the root and timeline parent.
    #[cfg(test)]
    pub(super) fn create(
        data_dir: &Path,
        timeline_relative: &Path,
    ) -> Result<Self, SessionPersistError> {
        Self::acquire(data_dir, timeline_relative, RootDisposition::Create)
    }

    /// Acquire a transaction within an existing root and timeline parent.
    pub(super) fn open(
        data_dir: &Path,
        timeline_relative: &Path,
    ) -> Result<Self, SessionPersistError> {
        Self::acquire(data_dir, timeline_relative, RootDisposition::Open)
    }

    pub(super) fn root(&self) -> &PrivateRoot {
        &self.root
    }

    fn acquire(
        data_dir: &Path,
        timeline_relative: &Path,
        disposition: RootDisposition,
    ) -> Result<Self, SessionPersistError> {
        let lock_relative = lock_relative(timeline_relative)?;
        let identity = data_dir.join(&lock_relative);
        let process_guard = lock_process_gate(&identity);
        let descriptor_permit = acquire_private_fs()?;
        let root = match disposition {
            #[cfg(test)]
            RootDisposition::Create => PrivateRoot::create(data_dir)?,
            RootDisposition::Open => PrivateRoot::open(data_dir)?,
        };
        root.create_dir_all(Path::new(TIMELINE_LOCK_DIRECTORY))?;
        let lock_file = root.open_lock(&lock_relative)?;
        lock_file.lock()?;
        Ok(Self {
            _lock: TimelineLockGuard {
                lock_file,
                _process_guard: process_guard,
            },
            root,
            _descriptor_permit: descriptor_permit,
        })
    }
}

/// A timeline lock acquired beneath a caller-owned admitted private root.
#[derive(Debug)]
pub(super) struct TimelineLockGuard {
    lock_file: File,
    _process_guard: ProcessTimelineGuard,
}

impl TimelineLockGuard {
    pub(super) fn acquire_under(
        root: &PrivateRoot,
        timeline_relative: &Path,
    ) -> Result<Self, SessionPersistError> {
        let lock_relative = lock_relative(timeline_relative)?;
        let identity = root.display_path(&lock_relative);
        let process_guard = lock_process_gate(&identity);
        root.create_dir_all(Path::new(TIMELINE_LOCK_DIRECTORY))?;
        let lock_file = root.open_lock(&lock_relative)?;
        lock_file.lock()?;
        Ok(Self {
            lock_file,
            _process_guard: process_guard,
        })
    }
}

impl Drop for TimelineLockGuard {
    fn drop(&mut self) {
        if let Err(error) = self.lock_file.unlock() {
            tracing::warn!(%error, "failed to explicitly unlock session timeline transaction");
        }
    }
}

/// A timeline file whose transaction remains held until the file is dropped.
#[derive(Debug)]
pub(crate) struct LockedTimelineFile {
    file: File,
    _authority: TimelineAuthority,
}

#[derive(Debug)]
enum TimelineAuthority {
    #[cfg(test)]
    Transaction { _transaction: TimelineTransaction },
    Registered {
        _timeline: TimelineLockGuard,
        _index: IndexLock,
    },
}

impl LockedTimelineFile {
    #[cfg(test)]
    pub(super) fn new(file: File, transaction: TimelineTransaction) -> Self {
        Self {
            file,
            _authority: TimelineAuthority::Transaction {
                _transaction: transaction,
            },
        }
    }

    pub(super) fn new_registered(
        file: File,
        timeline: TimelineLockGuard,
        index: IndexLock,
    ) -> Self {
        Self {
            file,
            _authority: TimelineAuthority::Registered {
                _timeline: timeline,
                _index: index,
            },
        }
    }
}

impl Deref for LockedTimelineFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl DerefMut for LockedTimelineFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum RootDisposition {
    Create,
    Open,
}

#[derive(Debug)]
struct ProcessTimelineGuard {
    identity: PathBuf,
}

impl Drop for ProcessTimelineGuard {
    fn drop(&mut self) {
        let mut active = PROCESS_TIMELINE_GATES.lock();
        active.remove(&self.identity);
        PROCESS_TIMELINE_GATE_CHANGED.notify_all();
    }
}

fn lock_process_gate(identity: &Path) -> ProcessTimelineGuard {
    let mut active = PROCESS_TIMELINE_GATES.lock();
    if active.insert(identity.to_path_buf()) {
        return ProcessTimelineGuard {
            identity: identity.to_path_buf(),
        };
    }
    #[cfg(test)]
    record_waiter(identity, true);
    while active.contains(identity) {
        PROCESS_TIMELINE_GATE_CHANGED.wait(&mut active);
    }
    active.insert(identity.to_path_buf());
    #[cfg(test)]
    record_waiter(identity, false);
    ProcessTimelineGuard {
        identity: identity.to_path_buf(),
    }
}

#[cfg(test)]
fn record_waiter(identity: &Path, started: bool) {
    let mut waiters = PROCESS_TIMELINE_WAITERS.lock();
    if started {
        let count = waiters.entry(identity.to_path_buf()).or_default();
        *count = count.saturating_add(1);
    } else if let Some(count) = waiters.get_mut(identity) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            waiters.remove(identity);
        }
    }
    PROCESS_TIMELINE_WAITER_CHANGED.notify_all();
}

fn lock_relative(timeline_relative: &Path) -> Result<PathBuf, SessionPersistError> {
    if timeline_relative.is_absolute()
        || !timeline_relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid_relative());
    }
    let mut hasher = Sha256::new();
    hasher.update(TIMELINE_LOCK_DOMAIN);
    let mut component_count = 0_u64;
    for component in timeline_relative.components() {
        let Component::Normal(component) = component else {
            return Err(invalid_relative());
        };
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt as _;
            component.as_bytes()
        };
        #[cfg(not(unix))]
        let bytes = component
            .to_str()
            .map(str::as_bytes)
            .ok_or_else(invalid_relative)?;
        let length = u64::try_from(bytes.len()).map_err(std::io::Error::other)?;
        hasher.update(length.to_be_bytes());
        hasher.update(bytes);
        component_count = component_count
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("timeline path component count overflow"))?;
    }
    if component_count == 0 {
        return Err(invalid_relative());
    }
    hasher.update(component_count.to_be_bytes());
    Ok(Path::new(TIMELINE_LOCK_DIRECTORY).join(format!("{:x}.lock", hasher.finalize())))
}

fn invalid_relative() -> SessionPersistError {
    SessionPersistError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "timeline transaction path must be non-empty, relative, and normalized",
    ))
}

#[cfg(test)]
pub(super) fn lock_timeline_for_test(
    data_dir: &Path,
    timeline_relative: &Path,
) -> Result<TimelineTransaction, SessionPersistError> {
    TimelineTransaction::create(data_dir, timeline_relative)
}

#[cfg(test)]
pub(super) fn wait_for_timeline_waiters_for_test(
    data_dir: &Path,
    timeline_relative: &Path,
    expected: usize,
) -> Result<(), SessionPersistError> {
    let identity = data_dir.join(lock_relative(timeline_relative)?);
    let mut waiters = PROCESS_TIMELINE_WAITERS.lock();
    while waiters.get(&identity).copied().unwrap_or_default() < expected {
        PROCESS_TIMELINE_WAITER_CHANGED.wait(&mut waiters);
    }
    Ok(())
}
