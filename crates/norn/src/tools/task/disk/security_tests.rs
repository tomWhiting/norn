use super::*;

fn entry(id: &str) -> TaskEntry {
    let now = Utc::now();
    TaskEntry {
        id: id.to_owned(),
        description: "private task".to_owned(),
        status: TaskStatus::Pending,
        depends_on: Vec::new(),
        metadata: Value::Null,
        created_at: now,
        updated_at: now,
        parent_task_id: None,
        assigned_agent: None,
    }
}

#[cfg(unix)]
#[test]
fn task_files_are_private_and_hostile_ids_cannot_escape() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::PermissionsExt as _;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("tasks");
    let store = DiskTaskStore::new(root_path.clone(), "group".to_owned());
    store.create(entry("_legal-task"))?;

    let mode = |path: &Path| -> std::io::Result<u32> {
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
    };
    assert_eq!(mode(&root_path)?, 0o700);
    assert_eq!(mode(&root_path.join("group"))?, 0o700);
    assert_eq!(mode(&root_path.join("group/_legal-task.json"))?, 0o600);

    std::fs::set_permissions(&root_path, std::fs::Permissions::from_mode(0o755))?;
    std::fs::set_permissions(
        root_path.join("group"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    std::fs::set_permissions(
        root_path.join("group/_legal-task.json"),
        std::fs::Permissions::from_mode(0o644),
    )?;
    assert_eq!(
        store
            .get("_legal-task")?
            .ok_or("task disappeared during read-only reopen")?
            .id,
        "_legal-task",
    );
    assert_eq!(mode(&root_path)?, 0o700);
    assert_eq!(mode(&root_path.join("group"))?, 0o700);
    assert_eq!(mode(&root_path.join("group/_legal-task.json"))?, 0o600);

    assert!(store.create(entry("../outside")).is_err());
    assert!(!container.path().join("outside.json").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn task_reads_and_rewrites_reject_links_without_touching_target()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("tasks");
    let group = root_path.join("group");
    std::fs::create_dir_all(&group)?;
    let target = container.path().join("outside.json");
    std::fs::write(&target, serde_json::to_vec(&entry("outside"))?)?;
    symlink(&target, group.join("linked.json"))?;
    let store = DiskTaskStore::new(root_path, "group".to_owned());

    assert!(
        store
            .update("linked", None, Some("changed".to_owned()), None, None)
            .is_err()
    );
    let persisted: TaskEntry = serde_json::from_slice(&std::fs::read(target)?)?;
    assert_eq!(persisted.id, "outside");
    assert_eq!(persisted.description, "private task");
    Ok(())
}

#[cfg(unix)]
#[test]
fn claim_transaction_remains_on_locked_root_after_root_replacement()
-> Result<(), Box<dyn std::error::Error>> {
    let container = tempfile::tempdir()?;
    let root_path = container.path().join("tasks");
    let parked = container.path().join("parked");
    let store = DiskTaskStore::new(root_path.clone(), "group".to_owned());
    store.create(entry("task"))?;

    let root = store.create_root()?;
    root.create_dir_all(&store.group_relative()?)?;
    let guard = LockGuard::acquire(root, store.lock_relative("task")?)?;
    std::fs::rename(&root_path, &parked)?;

    let replacement = DiskTaskStore::new(root_path.clone(), "group".to_owned());
    let mut replacement_entry = entry("task");
    replacement_entry.description = "replacement".to_owned();
    replacement.create(replacement_entry)?;

    let mut claimed = store.read_entry_in(guard.root(), "task")?;
    claimed.assigned_agent = Some("agent/pinned".to_owned());
    store.write_entry_atomic_in(guard.root(), &claimed, true)?;
    guard.release()?;

    let parked_entry: TaskEntry =
        serde_json::from_slice(&std::fs::read(parked.join("group/task.json"))?)?;
    let replacement_entry = replacement
        .get("task")?
        .ok_or("replacement task unexpectedly missing")?;
    assert_eq!(parked_entry.assigned_agent.as_deref(), Some("agent/pinned"));
    assert_eq!(replacement_entry.description, "replacement");
    assert_eq!(replacement_entry.assigned_agent, None);
    assert!(!root_path.join("group/task.lock").exists());
    assert!(!parked.join("group/task.lock").exists());
    Ok(())
}
