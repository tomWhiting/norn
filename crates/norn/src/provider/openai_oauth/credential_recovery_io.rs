//! Durable publication primitives for the refresh recovery journal.

use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use super::{JOURNAL_FILE, RecoveryJournalError, RefreshRecoveryMarker};
use crate::provider::openai_oauth::credential_transaction::CredentialTransaction;

impl CredentialTransaction {
    pub(super) fn replace_marker(
        &self,
        expected: Option<&RefreshRecoveryMarker>,
        proposed: &RefreshRecoveryMarker,
    ) -> Result<(), RecoveryJournalError> {
        if self.load_marker()?.as_ref() != expected {
            return Err(RecoveryJournalError::Changed);
        }
        let mut raw =
            serde_json::to_vec_pretty(proposed).map_err(RecoveryJournalError::Serialization)?;
        raw.push(b'\n');
        let temporary = marker_temporary_path();
        let result = (|| {
            #[cfg(test)]
            fail_if_armed(&self.root, RecoveryFaultPoint::MarkerCreate)
                .map_err(RecoveryJournalError::Io)?;
            let mut file = self
                .root
                .create_new(&temporary)
                .map_err(RecoveryJournalError::Io)?;
            file.write_all(&raw).map_err(RecoveryJournalError::Io)?;
            #[cfg(test)]
            fail_if_armed(&self.root, RecoveryFaultPoint::MarkerWriteSync)
                .map_err(RecoveryJournalError::Io)?;
            file.sync_all().map_err(RecoveryJournalError::Io)?;
            drop(file);
            #[cfg(test)]
            fail_if_armed(&self.root, RecoveryFaultPoint::MarkerRename)
                .map_err(RecoveryJournalError::Io)?;
            self.root
                .rename(&temporary, Path::new(JOURNAL_FILE))
                .map_err(RecoveryJournalError::Io)?;
            #[cfg(test)]
            fail_if_armed(&self.root, RecoveryFaultPoint::MarkerDirSync)
                .map_err(RecoveryJournalError::PublishedButUndurable)?;
            self.root
                .sync_dir(Path::new(""))
                .map_err(RecoveryJournalError::PublishedButUndurable)?;
            Ok(())
        })();
        if result.is_err() {
            self.cleanup_marker_temporary(&temporary);
        }
        result?;
        if self.load_marker()?.as_ref() != Some(proposed) {
            return Err(RecoveryJournalError::Changed);
        }
        Ok(())
    }

    pub(super) fn clear_marker(
        &self,
        expected: &RefreshRecoveryMarker,
    ) -> Result<(), RecoveryJournalError> {
        let Some(current) = self.load_marker()? else {
            return Ok(());
        };
        if &current != expected {
            return Err(RecoveryJournalError::Changed);
        }
        self.remove_marker_file()
    }

    pub(super) fn remove_marker_file(&self) -> Result<(), RecoveryJournalError> {
        #[cfg(test)]
        fail_if_armed(&self.root, RecoveryFaultPoint::MarkerDelete)
            .map_err(RecoveryJournalError::Io)?;
        self.root
            .remove_file(Path::new(JOURNAL_FILE))
            .map_err(RecoveryJournalError::Io)?;
        #[cfg(test)]
        fail_if_armed(&self.root, RecoveryFaultPoint::MarkerDeleteDirSync)
            .map_err(RecoveryJournalError::DeletedButUndurable)?;
        self.root
            .sync_dir(Path::new(""))
            .map_err(RecoveryJournalError::DeletedButUndurable)
    }

    fn cleanup_marker_temporary(&self, temporary: &Path) {
        match self.root.remove_file(temporary) {
            Ok(()) => {
                if let Err(error) = self.root.sync_dir(Path::new("")) {
                    tracing::warn!(
                        %error,
                        "OAuth refresh recovery temporary cleanup was not durable"
                    );
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, "OAuth refresh recovery temporary cleanup failed");
            }
        }
    }
}

fn marker_temporary_path() -> PathBuf {
    PathBuf::from(format!(
        "{JOURNAL_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::provider::openai_oauth) enum RecoveryFaultPoint {
    MarkerCreate,
    MarkerWriteSync,
    MarkerRename,
    MarkerDirSync,
    CredentialTempCreate,
    CredentialTempWrite,
    CredentialTempFileSync,
    CredentialFinalRename,
    CredentialParentDirSync,
    CredentialQuarantineRename,
    CredentialQuarantineRemove,
    CredentialPostDeleteDirSync,
    MarkerDelete,
    MarkerDeleteDirSync,
}

#[cfg(test)]
fn fail_if_armed(
    root: &crate::util::PrivateRoot,
    point: RecoveryFaultPoint,
) -> std::io::Result<()> {
    inject_recovery_fault(root, point)
}

#[cfg(test)]
pub(in crate::provider::openai_oauth) use injection::{arm_recovery_fault, inject_recovery_fault};

#[cfg(test)]
mod injection {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use parking_lot::Mutex;

    use super::RecoveryFaultPoint;

    #[derive(Debug)]
    struct ArmedFault {
        root: PathBuf,
        point: RecoveryFaultPoint,
        triggered: Arc<AtomicBool>,
    }

    static ARMED: Mutex<Vec<ArmedFault>> = Mutex::new(Vec::new());

    #[derive(Debug)]
    pub(in crate::provider::openai_oauth) struct RecoveryFaultGuard {
        triggered: Arc<AtomicBool>,
    }

    impl RecoveryFaultGuard {
        pub(in crate::provider::openai_oauth) fn was_triggered(&self) -> bool {
            self.triggered.load(Ordering::SeqCst)
        }
    }

    impl Drop for RecoveryFaultGuard {
        fn drop(&mut self) {
            ARMED
                .lock()
                .retain(|fault| !Arc::ptr_eq(&fault.triggered, &self.triggered));
        }
    }

    pub(in crate::provider::openai_oauth) fn arm_recovery_fault(
        root: &Path,
        point: RecoveryFaultPoint,
    ) -> RecoveryFaultGuard {
        let triggered = Arc::new(AtomicBool::new(false));
        ARMED.lock().push(ArmedFault {
            root: root.to_path_buf(),
            point,
            triggered: Arc::clone(&triggered),
        });
        RecoveryFaultGuard { triggered }
    }

    pub(in crate::provider::openai_oauth) fn inject_recovery_fault(
        root: &crate::util::PrivateRoot,
        point: RecoveryFaultPoint,
    ) -> std::io::Result<()> {
        let mut armed = ARMED.lock();
        let Some(index) = armed
            .iter()
            .position(|fault| fault.root == root.path() && fault.point == point)
        else {
            return Ok(());
        };
        let fault = armed.remove(index);
        fault.triggered.store(true, Ordering::SeqCst);
        Err(std::io::Error::other(format!(
            "injected OAuth recovery {point:?} fault"
        )))
    }
}
