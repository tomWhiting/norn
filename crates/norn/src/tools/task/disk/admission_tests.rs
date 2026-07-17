use std::sync::Arc;

use super::*;
use crate::resource::{DescriptorGovernor, PRIVATE_FS_OPERATION_PEAK};

fn entry(id: &str) -> TaskEntry {
    let now = Utc::now();
    TaskEntry {
        id: id.to_owned(),
        description: "governed task".to_owned(),
        status: TaskStatus::Pending,
        depends_on: Vec::new(),
        metadata: Value::Null,
        created_at: now,
        updated_at: now,
        parent_task_id: None,
        assigned_agent: None,
    }
}

fn governed_store(root: PathBuf, capacity: u32) -> (DiskTaskStore, Arc<DescriptorGovernor>) {
    let governor = Arc::new(DescriptorGovernor::with_capacity(capacity));
    let store = DiskTaskStore::with_governor(root, "group".to_owned(), Arc::clone(&governor));
    (store, governor)
}

fn require_admission_error<T>(result: Result<T, ToolError>) -> Result<(), std::io::Error> {
    match result {
        Err(ToolError::DescriptorAdmission(_)) => Ok(()),
        Err(error) => Err(std::io::Error::other(format!(
            "expected descriptor admission error, got {error}"
        ))),
        Ok(_) => Err(std::io::Error::other(
            "operation unexpectedly bypassed descriptor admission",
        )),
    }
}

#[test]
fn every_transaction_requires_the_full_private_filesystem_weight()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("tasks");
    let (store, _governor) = governed_store(root.clone(), PRIVATE_FS_OPERATION_PEAK - 1);

    require_admission_error(store.create(entry("new")))?;
    require_admission_error(store.get("missing"))?;
    require_admission_error(store.list(None))?;
    require_admission_error(store.update("missing", None, None, None, None))?;
    require_admission_error(store.complete("missing"))?;
    require_admission_error(store.create_subtask("missing", entry("child")))?;
    require_admission_error(store.children("missing"))?;
    require_admission_error(store.ancestors("missing"))?;
    require_admission_error(store.claim("missing", "root/worker"))?;
    require_admission_error(store.create_group("new-group"))?;
    require_admission_error(store.list_groups())?;

    assert!(!root.exists(), "denied transactions must not touch storage");
    Ok(())
}

#[test]
fn exact_weight_supports_nested_work_without_nested_admission()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("tasks");
    let (store, _governor) = governed_store(root.clone(), PRIVATE_FS_OPERATION_PEAK);

    store.create(entry("task"))?;
    let completed = store.complete("task")?;
    assert_eq!(completed.status, TaskStatus::Completed);
    let claimed = store.claim("task", "root/worker")?;
    assert_eq!(claimed.assigned_agent.as_deref(), Some("root/worker"));
    assert!(!root.join("group/task.lock").exists());

    let loaded = store.get("task")?.ok_or("published task missing")?;
    assert_eq!(loaded.status, TaskStatus::Completed);
    assert_eq!(loaded.assigned_agent.as_deref(), Some("root/worker"));
    Ok(())
}

#[test]
fn missing_storage_is_distinct_from_storage_failure() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("missing");
    let (store, _governor) = governed_store(root, PRIVATE_FS_OPERATION_PEAK);

    assert!(store.get("task")?.is_none());
    assert!(store.list(None)?.is_empty());
    assert!(store.children("task")?.is_empty());
    assert!(store.list_groups()?.is_empty());

    let blocked_root = temporary.path().join("blocked");
    std::fs::write(&blocked_root, b"not a directory")?;
    let (blocked, _governor) = governed_store(blocked_root, PRIVATE_FS_OPERATION_PEAK);
    assert!(blocked.list_groups().is_err());
    Ok(())
}

#[test]
fn corrupt_task_data_propagates_instead_of_looking_empty() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("tasks");
    let (store, _governor) = governed_store(root.clone(), PRIVATE_FS_OPERATION_PEAK);
    store.create(entry("task"))?;
    std::fs::write(root.join("group/task.json"), b"{broken")?;

    assert!(store.get("task").is_err());
    assert!(store.list(None).is_err());
    assert!(store.children("parent").is_err());
    assert!(store.ancestors("task").is_err());
    Ok(())
}
