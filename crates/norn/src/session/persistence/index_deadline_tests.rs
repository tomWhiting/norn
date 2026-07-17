use std::time::Duration;

use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::store::DurabilityPolicy;

use super::types::SessionPersistError;

#[test]
fn manager_reads_respect_configured_index_lock_deadline() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let setup = SessionManager::new(directory.path());
    let opened = setup.create(
        CreateSessionOptions {
            model: "gpt-test".to_owned(),
            working_dir: "/work".to_owned(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let id = opened.entry.id.clone();
    drop(opened);

    let deadline = Duration::from_millis(50);
    let manager = SessionManager::new(directory.path()).with_index_lock_deadline(Some(deadline));
    let held = super::lock::lock_index(directory.path(), None)?;

    let list_error = manager
        .list()
        .err()
        .ok_or_else(|| std::io::Error::other("manager list ignored its index-lock deadline"))?;
    assert!(matches!(
        list_error,
        SessionPersistError::IndexLockTimeout { waited, .. } if waited == deadline
    ));

    let resolve_error = manager
        .resolve(&id)
        .err()
        .ok_or_else(|| std::io::Error::other("manager resolve ignored its index-lock deadline"))?;
    assert!(matches!(
        resolve_error,
        SessionPersistError::IndexLockTimeout { waited, .. } if waited == deadline
    ));

    drop(held);
    assert_eq!(manager.list()?.len(), 1);
    assert_eq!(manager.resolve(&id)?.id, id);
    Ok(())
}
