//! Process-wide weighted admission for Norn-owned active descriptors.

use std::fmt;
use std::sync::{Arc, OnceLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{DescriptorSnapshot, descriptor_snapshot};

/// Exact reserve for the descriptor observer opened by `descriptor_snapshot`.
/// Every Norn-owned filesystem, subprocess, pipe, transport, and socket family
/// consumes its own weighted admission instead of relying on extra headroom.
const DESCRIPTOR_OBSERVER_RESERVE: u64 = 1;

/// Parent-side peak for a child with piped stdout/stderr and null stdin:
/// four stdio pipe ends, `/dev/null`, and both Unix exec-status pipe ends.
pub(crate) const TWO_PIPE_SPAWN_PEAK: u32 = 7;

/// Parent-side peak for a child with one output pipe, null stdin, inherited
/// stderr: two stdio pipe ends, `/dev/null`, and both exec-status pipe ends.
pub(crate) const ONE_PIPE_SPAWN_PEAK: u32 = 5;

/// Parent-side peak for a child with all three standard streams piped:
/// six stdio pipe ends plus both Unix exec-status pipe ends.
pub(crate) const THREE_PIPE_SPAWN_PEAK: u32 = 8;

/// Parent-side stdin/stdout/stderr handles retained after an all-piped child
/// has spawned successfully.
pub(crate) const THREE_PIPE_RETAINED: u32 = 3;

/// Active request allowance for resolver/connect/socket/TLS ownership.
pub(crate) const HTTP_REQUEST_PEAK: u32 = 3;

/// Peak for one descriptor-relative private-filesystem transaction: the
/// private root, two simultaneously held traversal directories, a source file,
/// and a no-replace publication destination. Simpler private reads and writes
/// use the same conservative operation-scoped reservation.
pub(crate) const PRIVATE_FS_OPERATION_PEAK: u32 = 5;

/// Serial `ignore::Walk` peak: `walkdir` 2.5.0 retains at most ten directory
/// handles (`WalkDirOptions::max_open`) while `ignore` may open one ignore file
/// when constructing the matcher for the current directory.
pub(crate) const RECURSIVE_WALK_PEAK: u32 = 11;

/// Child with null stdin and captured stdout/stderr: four pipe ends, one
/// `/dev/null` handle, and both Unix exec-status pipe ends.
pub(crate) const OUTPUT_SUBPROCESS_PEAK: u32 = 7;

/// Child with all three standard streams attached to `/dev/null`: three null
/// handles and both Unix exec-status pipe ends.
pub(crate) const NULL_STDIO_SUBPROCESS_PEAK: u32 = 5;

static GLOBAL: OnceLock<Arc<DescriptorGovernor>> = OnceLock::new();

/// Failure to establish or enter the active-descriptor budget.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct DescriptorAdmissionError {
    requested: u32,
    capacity: Option<u32>,
    reason: String,
    snapshot: Box<DescriptorSnapshot>,
}

impl fmt::Display for DescriptorAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot admit {} active file descriptors",
            self.requested
        )?;
        if let Some(capacity) = self.capacity {
            write!(formatter, "; Norn active capacity is {capacity}")?;
        }
        write!(formatter, ": {}; run `norn doctor`", self.reason)
    }
}

impl std::error::Error for DescriptorAdmissionError {}

/// One process-wide weighted descriptor budget.
#[derive(Debug)]
pub(crate) struct DescriptorGovernor {
    capacity: u32,
    semaphore: Arc<Semaphore>,
}

impl DescriptorGovernor {
    /// Return the lazily initialized process-wide governor.
    pub(crate) fn global() -> Result<Arc<Self>, DescriptorAdmissionError> {
        if let Some(governor) = GLOBAL.get() {
            return Ok(Arc::clone(governor));
        }
        let snapshot = descriptor_snapshot();
        let candidate = Arc::new(Self::from_snapshot(&snapshot)?);
        match GLOBAL.set(Arc::clone(&candidate)) {
            Ok(()) => Ok(candidate),
            Err(_already_initialized) => GLOBAL.get().map(Arc::clone).ok_or_else(|| {
                initialization_error("descriptor governor initialization raced", &snapshot)
            }),
        }
    }

    fn from_snapshot(snapshot: &DescriptorSnapshot) -> Result<Self, DescriptorAdmissionError> {
        let limits = snapshot
            .limits
            .ok_or_else(|| initialization_error("descriptor limits are unavailable", snapshot))?;
        let open = snapshot
            .open
            .as_ref()
            .map(|observation| observation.count)
            .ok_or_else(|| {
                initialization_error("open descriptor count is unavailable", snapshot)
            })?;
        let maximum =
            u64::try_from(Semaphore::MAX_PERMITS).map_err(|error| DescriptorAdmissionError {
                requested: 0,
                capacity: None,
                reason: format!("semaphore capacity is not representable: {error}"),
                snapshot: Box::new(snapshot.clone()),
            })?;
        // `None` is the OS-reported infinity sentinel, not an observation
        // failure. In that case there is no per-process EMFILE ceiling to
        // subtract from; cap only at the semaphore representation. A
        // process-local authority cannot prevent system-wide ENFILE.
        let ceiling = limits.soft.unwrap_or(maximum);
        let capacity = ceiling
            .saturating_sub(open)
            .saturating_sub(DESCRIPTOR_OBSERVER_RESERVE);
        let capacity = capacity.min(maximum).min(u64::from(u32::MAX));
        let capacity = u32::try_from(capacity).map_err(|error| DescriptorAdmissionError {
            requested: 0,
            capacity: None,
            reason: format!("safe descriptor capacity is not representable: {error}"),
            snapshot: Box::new(snapshot.clone()),
        })?;
        if capacity == 0 {
            return Err(initialization_error(
                "no capacity remains after current usage and transient headroom",
                snapshot,
            ));
        }
        Ok(Self {
            capacity,
            semaphore: Arc::new(Semaphore::new(capacity as usize)),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_capacity(capacity: u32) -> Self {
        Self {
            capacity,
            semaphore: Arc::new(Semaphore::new(capacity as usize)),
        }
    }

    /// Wait for `weight` descriptors, releasing them when the permit drops.
    #[cfg(test)]
    pub(crate) async fn acquire(
        &self,
        weight: u32,
    ) -> Result<DescriptorPermit, DescriptorAdmissionError> {
        if weight == 0 || weight > self.capacity {
            return Err(self.invalid_weight(weight));
        }
        let permit = Arc::clone(&self.semaphore)
            .acquire_many_owned(weight)
            .await
            .map_err(|error| DescriptorAdmissionError {
                requested: weight,
                capacity: Some(self.capacity),
                reason: format!("descriptor governor closed unexpectedly: {error}"),
                snapshot: Box::new(descriptor_snapshot()),
            })?;
        Ok(DescriptorPermit { permit })
    }

    /// Fail-fast admission for synchronous subprocess and HTTP boundaries.
    pub(crate) fn try_acquire(
        &self,
        weight: u32,
    ) -> Result<DescriptorPermit, DescriptorAdmissionError> {
        self.try_acquire_with_snapshot(weight, descriptor_snapshot())
    }

    fn try_acquire_with_snapshot(
        &self,
        weight: u32,
        snapshot: DescriptorSnapshot,
    ) -> Result<DescriptorPermit, DescriptorAdmissionError> {
        if weight == 0 || weight > self.capacity {
            return Err(self.invalid_weight(weight));
        }
        let permit = Arc::clone(&self.semaphore)
            .try_acquire_many_owned(weight)
            .map_err(|error| DescriptorAdmissionError {
                requested: weight,
                capacity: Some(self.capacity),
                reason: format!("safe active descriptor capacity is busy: {error}"),
                snapshot: Box::new(snapshot.clone()),
            })?;
        let limits = snapshot.limits;
        let open = snapshot.open.as_ref().map(|observation| observation.count);
        let available = u32::try_from(self.semaphore.available_permits()).unwrap_or(0);
        let reserved = u64::from(self.capacity.saturating_sub(available));
        let live_safe = limits.zip(open).is_some_and(|(limits, open)| {
            limits.soft.is_none_or(|soft| {
                open.saturating_add(reserved)
                    .saturating_add(DESCRIPTOR_OBSERVER_RESERVE)
                    <= soft
            })
        });
        if !live_safe {
            return Err(DescriptorAdmissionError {
                requested: weight,
                capacity: Some(self.capacity),
                reason: "live descriptor usage no longer fits the safe admitted budget".to_owned(),
                snapshot: Box::new(snapshot),
            });
        }
        Ok(DescriptorPermit { permit })
    }

    fn invalid_weight(&self, weight: u32) -> DescriptorAdmissionError {
        DescriptorAdmissionError {
            requested: weight,
            capacity: Some(self.capacity),
            reason: if weight == 0 {
                "descriptor weight must be positive".to_owned()
            } else {
                "one operation exceeds the entire safe active budget".to_owned()
            },
            snapshot: Box::new(descriptor_snapshot()),
        }
    }

    #[cfg(test)]
    pub(crate) fn available(&self) -> usize {
        self.semaphore.available_permits()
    }
}

/// Owned weighted admission; capacity returns automatically on drop.
#[derive(Debug)]
pub(crate) struct DescriptorPermit {
    permit: OwnedSemaphorePermit,
}

impl DescriptorPermit {
    /// Split `weight` descriptors into an independently owned lifetime.
    pub(crate) fn split(&mut self, weight: u32) -> Option<Self> {
        self.permit
            .split(weight as usize)
            .map(|permit| Self { permit })
    }
}

fn initialization_error(reason: &str, snapshot: &DescriptorSnapshot) -> DescriptorAdmissionError {
    DescriptorAdmissionError {
        requested: 0,
        capacity: None,
        reason: reason.to_owned(),
        snapshot: Box::new(snapshot.clone()),
    }
}

#[cfg(test)]
#[path = "descriptor_governor_tests.rs"]
mod tests;
