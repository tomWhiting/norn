use super::*;

fn entry(id: &str, status: TaskStatus) -> TaskEntry {
    let now = Utc::now();
    TaskEntry {
        id: id.to_string(),
        description: format!("task {id}"),
        status,
        depends_on: vec![],
        metadata: Value::Null,
        created_at: now,
        updated_at: now,
        parent_task_id: None,
        assigned_agent: None,
    }
}

fn store(tmp: &tempfile::TempDir, slug: &str) -> DiskTaskStore {
    DiskTaskStore::new(tmp.path().to_path_buf(), slug.to_string())
}

fn execution_failure_reason<T>(result: Result<T, ToolError>) -> Result<String, std::io::Error> {
    match result {
        Err(ToolError::ExecutionFailed { reason }) => Ok(reason),
        Err(error) => Err(std::io::Error::other(format!(
            "expected ExecutionFailed, got {error}"
        ))),
        Ok(_) => Err(std::io::Error::other("operation unexpectedly succeeded")),
    }
}

#[test]
fn create_get_list_update_complete_cycle() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "g1");

    store.create(entry("t1", TaskStatus::Pending))?;
    store.create(entry("t2", TaskStatus::Pending))?;

    let got = store.get("t1")?.ok_or("task t1 missing")?;
    assert_eq!(got.id, "t1");

    let all = store.list(None)?;
    assert_eq!(all.len(), 2);

    let updated = store.update("t1", Some(TaskStatus::InProgress), None, None, None)?;
    assert_eq!(updated.status, TaskStatus::InProgress);
    let in_progress = store.list(Some(TaskStatus::InProgress))?;
    assert_eq!(in_progress.len(), 1);
    assert_eq!(in_progress[0].id, "t1");

    let completed = store.complete("t1")?;
    assert_eq!(completed.status, TaskStatus::Completed);
    let still_two = store.list(None)?;
    assert_eq!(still_two.len(), 2);
    Ok(())
}

#[test]
fn lazy_directory_creation_does_not_touch_fs_on_construction()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let _store = store(&tmp, "lazy");
    assert!(
        !tmp.path().join("lazy").exists(),
        "construction must not mkdir the group directory",
    );
    Ok(())
}

#[test]
fn directory_created_on_first_write() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "first-write");
    store.create(entry("a", TaskStatus::Pending))?;
    assert!(tmp.path().join("first-write").exists());
    assert!(tmp.path().join("first-write").join("a.json").exists());
    Ok(())
}

#[test]
fn create_subtask_writes_parent_link_and_children_lists_them()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "hier");
    store.create(entry("root", TaskStatus::Pending))?;
    store.create_subtask("root", entry("c1", TaskStatus::Pending))?;
    store.create_subtask("root", entry("c2", TaskStatus::Pending))?;
    let kids = store.children("root")?;
    assert_eq!(kids.len(), 2);
    for kid in &kids {
        assert_eq!(kid.parent_task_id.as_deref(), Some("root"));
    }
    Ok(())
}

#[test]
fn three_level_hierarchy_on_disk_walks_ancestors() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "ladder");
    store.create(entry("root", TaskStatus::Pending))?;
    store.create_subtask("root", entry("mid", TaskStatus::Pending))?;
    store.create_subtask("mid", entry("leaf", TaskStatus::Pending))?;
    assert_eq!(store.children("root")?.len(), 1);
    let chain = store.ancestors("leaf")?;
    let ids: Vec<&str> = chain.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["leaf", "mid", "root"]);
    Ok(())
}

#[test]
fn create_subtask_missing_parent_errors() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "orphan");
    let reason = execution_failure_reason(
        store.create_subtask("ghost", entry("child", TaskStatus::Pending)),
    )?;
    assert!(reason.contains("ghost"), "{reason}");
    Ok(())
}

#[test]
fn first_claim_succeeds_writes_assigned_agent() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "claims");
    store.create(entry("t1", TaskStatus::Pending))?;
    let claimed = store.claim("t1", "root/worker-a")?;
    assert_eq!(claimed.assigned_agent.as_deref(), Some("root/worker-a"));
    let on_disk = store.get("t1")?.ok_or("claimed task missing")?;
    assert_eq!(on_disk.assigned_agent.as_deref(), Some("root/worker-a"));
    Ok(())
}

#[test]
fn second_claim_fails_with_already_claimed_and_removes_lock()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "claims2");
    store.create(entry("t1", TaskStatus::Pending))?;
    store.claim("t1", "root/worker-a")?;
    let reason = execution_failure_reason(store.claim("t1", "root/worker-b"))?;
    assert!(reason.contains("already claimed"), "{reason}");
    let lock = tmp.path().join("claims2").join("t1.lock");
    assert!(!lock.exists(), "lock file must be removed after failure");
    Ok(())
}

#[test]
fn lock_removed_after_successful_claim() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "claims3");
    store.create(entry("t1", TaskStatus::Pending))?;
    store.claim("t1", "root/worker-a")?;
    let lock = tmp.path().join("claims3").join("t1.lock");
    assert!(!lock.exists(), "lock file must be removed after success");
    Ok(())
}

#[test]
fn claim_contended_returns_contended_message() -> Result<(), Box<dyn std::error::Error>> {
    // Pre-create the lock file to simulate a concurrent claim in progress.
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "contend");
    store.create(entry("t1", TaskStatus::Pending))?;

    let dir = tmp.path().join("contend");
    let lock = dir.join("t1.lock");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)?;

    let reason = execution_failure_reason(store.claim("t1", "root/worker"))?;
    assert!(reason.contains("contended"), "{reason}");
    // The pre-existing lock must NOT be removed by the failed claim —
    // it belongs to whatever else created it.
    assert!(lock.exists(), "external lock must not be removed");
    // Clean up so the tempdir Drop doesn't trip on stragglers.
    let _ = fs::remove_file(&lock);
    Ok(())
}

#[test]
fn create_group_and_list_groups_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "main");
    store.create_group("norn-agents-wiring")?;
    store.create_group("implement-hooks")?;
    // Idempotent: creating again is OK.
    store.create_group("implement-hooks")?;
    let groups = store.list_groups()?;
    assert_eq!(groups, vec!["implement-hooks", "norn-agents-wiring"]);
    Ok(())
}

#[test]
fn list_groups_on_missing_root_returns_empty() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = DiskTaskStore::new(tmp.path().join("does-not-exist"), "anything".to_string());
    assert!(store.list_groups()?.is_empty());
    Ok(())
}

#[test]
fn invalid_slugs_rejected() {
    assert!(validate_slug("has/slash").is_err());
    assert!(validate_slug("..").is_err());
    assert!(validate_slug("dot.dot").is_err());
    assert!(validate_slug("space here").is_err());
    assert!(validate_slug("").is_err());
    assert!(validate_slug("ok-slug_1").is_ok());
}

#[test]
fn create_group_rejects_invalid_slug() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "main");
    assert!(store.create_group("has/slash").is_err());
    assert!(store.create_group("..").is_err());
    Ok(())
}

#[test]
fn list_skips_tmp_and_lock_files() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let store = store(&tmp, "noise");
    store.create(entry("t1", TaskStatus::Pending))?;
    // Drop stray .tmp and .lock files in the group dir.
    let dir = tmp.path().join("noise");
    fs::write(dir.join("garbage.tmp.abc"), b"junk")?;
    fs::write(dir.join("garbage.lock"), b"")?;
    let listed = store.list(None)?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "t1");
    Ok(())
}

#[test]
fn data_survives_dropping_and_reconstructing_store() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    {
        let store = DiskTaskStore::new(root.clone(), "persist".to_string());
        store.create(entry("t1", TaskStatus::InProgress))?;
        store.create_subtask("t1", entry("t1-child", TaskStatus::Pending))?;
    }
    let reopened = DiskTaskStore::new(root, "persist".to_string());
    let all = reopened.list(None)?;
    assert_eq!(all.len(), 2);
    let kids = reopened.children("t1")?;
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].id, "t1-child");
    Ok(())
}
