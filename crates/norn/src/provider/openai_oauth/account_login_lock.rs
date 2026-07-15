//! Exclusive browser-login reservation for one named credential slot.

use std::collections::HashSet;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use parking_lot::{Condvar, Mutex};

use super::super::auth_root::NornAuthRoot;
use super::super::credential_lock_timing::CredentialLockTiming;
use super::{AccountCatalogError, LOGIN_LOCK_FILE};
use crate::resource::DescriptorPermit;
use crate::util::PrivateRoot;

static LOGIN_GATES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static LOGIN_GATE_CHANGED: Condvar = Condvar::new();

/// Guard preventing `logout --all` from crossing a legacy browser login.
pub struct DefaultLoginReservation {
    _slot_lock: LoginSlotLock,
}

impl std::fmt::Debug for DefaultLoginReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultLoginReservation")
            .finish_non_exhaustive()
    }
}

pub(super) struct LoginSlotLock {
    file: File,
    _process_guard: LoginProcessGuard,
    _descriptor_permit: DescriptorPermit,
}

impl LoginSlotLock {
    pub(super) fn acquire(
        auth_root: &NornAuthRoot,
        timing: CredentialLockTiming,
    ) -> Result<Self, AccountCatalogError> {
        Self::acquire_named(auth_root, timing, LOGIN_LOCK_FILE)
    }

    pub(super) fn acquire_named(
        auth_root: &NornAuthRoot,
        timing: CredentialLockTiming,
        lock_file: &str,
    ) -> Result<Self, AccountCatalogError> {
        let started = Instant::now();
        let identity = auth_root.as_path().join(lock_file);
        let process_guard = lock_process_gate(&identity, timing, started)?;
        let descriptor_permit = crate::resource::acquire_private_fs()
            .map_err(|error| AccountCatalogError::coordination(&error))?;
        let root = PrivateRoot::create_with_durable_ancestors(auth_root.as_path())?;
        let file = root.open_lock(Path::new(lock_file))?;
        loop {
            if started.elapsed() >= timing.deadline() {
                return Err(AccountCatalogError::LockTimeout);
            }
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    let remaining = timing
                        .deadline()
                        .saturating_sub(started.elapsed())
                        .min(timing.poll_interval());
                    std::thread::sleep(remaining);
                }
                Err(TryLockError::Error(error)) => {
                    return Err(AccountCatalogError::coordination(&error));
                }
            }
        }
        Ok(Self {
            file,
            _process_guard: process_guard,
            _descriptor_permit: descriptor_permit,
        })
    }
}

pub(super) fn default_login_reservation(
    auth_root: &NornAuthRoot,
    timing: CredentialLockTiming,
) -> Result<DefaultLoginReservation, AccountCatalogError> {
    LoginSlotLock::acquire(auth_root, timing).map(|slot_lock| DefaultLoginReservation {
        _slot_lock: slot_lock,
    })
}

impl Drop for LoginSlotLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!(%error, "failed to unlock named OAuth login reservation");
        }
    }
}

struct LoginProcessGuard {
    identity: PathBuf,
}

impl Drop for LoginProcessGuard {
    fn drop(&mut self) {
        let mut active = LOGIN_GATES.lock();
        active.remove(&self.identity);
        LOGIN_GATE_CHANGED.notify_all();
    }
}

fn lock_process_gate(
    identity: &Path,
    timing: CredentialLockTiming,
    started: Instant,
) -> Result<LoginProcessGuard, AccountCatalogError> {
    let mut active = LOGIN_GATES.lock();
    loop {
        if started.elapsed() >= timing.deadline() {
            return Err(AccountCatalogError::LockTimeout);
        }
        if active.insert(identity.to_path_buf()) {
            return Ok(LoginProcessGuard {
                identity: identity.to_path_buf(),
            });
        }
        let remaining = timing.deadline().saturating_sub(started.elapsed());
        let timeout = LOGIN_GATE_CHANGED.wait_for(&mut active, remaining);
        if timeout.timed_out() && active.contains(identity) {
            return Err(AccountCatalogError::LockTimeout);
        }
    }
}
