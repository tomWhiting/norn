use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use norn_policy::{EntryKind, PolicyState, RepositoryPath, evaluate_p1};

use super::RepositorySnapshotAdapter;
use super::error::SnapshotAdapterError;
use super::git::{GitInventory, parse_base_tree};
use super::workspace::WorkspaceRoot;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn git_inventory_parsers_keep_modes_and_reject_unmerged_rows() -> TestResult {
    const OBJECT: &str = "0000000000000000000000000000000000000000";
    let tracked = format!(
        "H 100755 {OBJECT} 0\texecutable.sh\0\
         H 120000 {OBJECT} 0\tlinked\0"
    );
    let inventory = GitInventory::parse(tracked.as_bytes(), b"visible.txt\0", false)?;
    let paths = inventory
        .paths()
        .map(RepositoryPath::as_str)
        .collect::<Vec<_>>();
    assert_eq!(paths, ["executable.sh", "linked", "visible.txt"]);

    let unmerged = format!("H 100644 {OBJECT} 2\tconflicted.rs\0");
    assert!(matches!(
        GitInventory::parse(unmerged.as_bytes(), b"", false),
        Err(SnapshotAdapterError::UnmergedIndex)
    ));
    assert!(matches!(
        GitInventory::parse(
            b"H 160000 0000000000000000000000000000000000000000 0\tsubmodule\0",
            b"",
            false
        ),
        Err(SnapshotAdapterError::GitEntry)
    ));
    assert!(matches!(
        GitInventory::parse(
            b"S 100644 0000000000000000000000000000000000000000 0\tsparse.rs\0",
            b"",
            false
        ),
        Err(SnapshotAdapterError::SparseIndex)
    ));
    Ok(())
}

#[test]
fn base_tree_parser_preserves_exact_leaf_modes() -> TestResult {
    const OBJECT: &str = "0000000000000000000000000000000000000000";
    let raw = format!(
        "100644 blob {OBJECT}\tregular.rs\0\
         100755 blob {OBJECT}\texecutable.sh\0\
         120000 blob {OBJECT}\tlinked\0"
    );
    let records = parse_base_tree(raw.as_bytes())?;
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].path.as_str(), "regular.rs");
    assert_eq!(records[1].path.as_str(), "executable.sh");
    assert_eq!(records[2].path.as_str(), "linked");
    assert!(matches!(
        parse_base_tree(format!("160000 commit {OBJECT}\tmodule\0").as_bytes()),
        Err(SnapshotAdapterError::GitEntry)
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn current_snapshot_is_exact_for_tracked_untracked_ignored_and_symlink_entries() -> TestResult {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let repository = initialized_repository()?;
    fs::write(repository.path().join("tracked.txt"), b"tracked")?;
    fs::write(repository.path().join("executable.sh"), b"#!/bin/sh\n")?;
    let mut permissions = fs::metadata(repository.path().join("executable.sh"))?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(repository.path().join("executable.sh"), permissions)?;
    symlink("missing-target", repository.path().join("linked"))?;
    fs::write(repository.path().join(".gitignore"), b"*.ignored\n")?;
    git(repository.path(), &["add", "--", "."])?;
    git(repository.path(), &["commit", "-q", "-m", "fixture base"])?;
    fs::write(repository.path().join("visible.txt"), b"visible")?;
    fs::write(repository.path().join("hidden.ignored"), b"hidden")?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_current()?;
    let snapshot = acquired.snapshot().snapshot();
    assert_entry(snapshot, "tracked.txt", EntryKind::Regular, b"tracked")?;
    assert_entry(
        snapshot,
        "executable.sh",
        EntryKind::Regular,
        b"#!/bin/sh\n",
    )?;
    assert_entry(snapshot, "linked", EntryKind::Symlink, b"missing-target")?;
    assert_entry(snapshot, "visible.txt", EntryKind::Regular, b"visible")?;
    assert!(!snapshot.contains_path(&RepositoryPath::parse("hidden.ignored")?));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn unprofiled_repository_is_absent_without_the_ratified_base_object() -> TestResult {
    let repository = initialized_repository()?;
    fs::write(repository.path().join("ordinary.txt"), b"ordinary")?;
    git(repository.path(), &["add", "--", "ordinary.txt"])?;
    git(
        repository.path(),
        &["commit", "-q", "-m", "ordinary repository"],
    )?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_p1()?;
    assert!(acquired.base_failure().is_none());
    assert!(matches!(
        evaluate_p1(acquired.evaluation_input()),
        PolicyState::Absent
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn unprofiled_sha256_repository_is_also_absent() -> TestResult {
    let base = repository_local_temp_root()?;
    let repository = tempfile::Builder::new()
        .prefix("norn-repository-snapshot-sha256-")
        .tempdir_in(base)?;
    git(repository.path(), &["init", "-q", "--object-format=sha256"])?;
    git(
        repository.path(),
        &["config", "user.name", "Synthetic Fixture"],
    )?;
    git(
        repository.path(),
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    fs::write(repository.path().join("ordinary.txt"), b"ordinary")?;
    git(repository.path(), &["add", "--", "ordinary.txt"])?;
    git(
        repository.path(),
        &["commit", "-q", "-m", "ordinary repository"],
    )?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_p1()?;
    assert!(acquired.base_failure().is_none());
    assert!(matches!(
        evaluate_p1(acquired.evaluation_input()),
        PolicyState::Absent
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn stable_tracked_deletion_is_an_absence_not_a_partial_snapshot() -> TestResult {
    let repository = initialized_repository()?;
    fs::write(repository.path().join("deleted.txt"), b"before")?;
    git(repository.path(), &["add", "--", "deleted.txt"])?;
    git(repository.path(), &["commit", "-q", "-m", "tracked file"])?;
    fs::remove_file(repository.path().join("deleted.txt"))?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_current()?;
    assert!(
        !acquired
            .snapshot()
            .snapshot()
            .contains_path(&RepositoryPath::parse("deleted.txt")?)
    );
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn tracked_phase_lock_deletion_remains_an_invalid_required_profile() -> TestResult {
    let repository = initialized_repository()?;
    let policy = repository.path().join("policy");
    fs::create_dir(&policy)?;
    fs::write(policy.join("phase-lock.json"), b"{}")?;
    git(repository.path(), &["add", "--", "policy/phase-lock.json"])?;
    git(repository.path(), &["commit", "-q", "-m", "policy marker"])?;
    fs::remove_file(policy.join("phase-lock.json"))?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_p1()?;
    assert!(acquired.base_failure().is_some());
    assert!(matches!(
        evaluate_p1(acquired.evaluation_input()),
        PolicyState::Invalid(_)
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn staged_phase_lock_deletion_remains_an_invalid_required_profile() -> TestResult {
    let repository = repository_with_committed_marker()?;
    git(
        repository.path(),
        &["rm", "-q", "--", "policy/phase-lock.json"],
    )?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_p1()?;
    assert!(acquired.base_failure().is_some());
    assert!(matches!(
        evaluate_p1(acquired.evaluation_input()),
        PolicyState::Invalid(_)
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn committed_phase_lock_deletion_remains_an_invalid_required_profile() -> TestResult {
    let repository = repository_with_committed_marker()?;
    git(
        repository.path(),
        &["rm", "-q", "--", "policy/phase-lock.json"],
    )?;
    git(repository.path(), &["commit", "-q", "-m", "remove marker"])?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_p1()?;
    assert!(acquired.base_failure().is_some());
    assert!(matches!(
        evaluate_p1(acquired.evaluation_input()),
        PolicyState::Invalid(_)
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn skip_worktree_entries_fail_closed_before_snapshot_construction() -> TestResult {
    let repository = initialized_repository()?;
    fs::write(repository.path().join("tracked.txt"), b"tracked")?;
    git(repository.path(), &["add", "--", "tracked.txt"])?;
    git(repository.path(), &["commit", "-q", "-m", "tracked file"])?;
    git(
        repository.path(),
        &["update-index", "--skip-worktree", "tracked.txt"],
    )?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    assert!(matches!(
        adapter.acquire_current(),
        Err(SnapshotAdapterError::SparseIndex)
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn revalidation_detects_content_and_inventory_changes_without_retrying() -> TestResult {
    let repository = initialized_repository()?;
    fs::write(repository.path().join("tracked.txt"), b"before")?;
    git(repository.path(), &["add", "--", "tracked.txt"])?;
    git(repository.path(), &["commit", "-q", "-m", "tracked file"])?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    let acquired = adapter.acquire_current()?;
    fs::write(repository.path().join("tracked.txt"), b"after")?;
    assert!(matches!(
        adapter.revalidate_current(&acquired),
        Err(SnapshotAdapterError::SnapshotChanged)
    ));

    let acquired = adapter.acquire_current()?;
    fs::write(repository.path().join("new.txt"), b"new")?;
    assert!(matches!(
        adapter.revalidate_current(&acquired),
        Err(SnapshotAdapterError::SnapshotChanged)
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn settled_root_entry_change_does_not_invalidate_the_pinned_directory() -> TestResult {
    let repository = initialized_repository()?;
    fs::write(repository.path().join("tracked.txt"), b"tracked")?;
    git(repository.path(), &["add", "--", "tracked.txt"])?;
    git(repository.path(), &["commit", "-q", "-m", "tracked file"])?;

    let adapter = RepositorySnapshotAdapter::discover(repository.path())?;
    fs::write(repository.path().join("settled.txt"), b"settled")?;
    let acquired = adapter.acquire_current()?;
    assert_entry(
        acquired.snapshot().snapshot(),
        "settled.txt",
        EntryKind::Regular,
        b"settled",
    )?;
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn adapter_rejects_a_different_repository_at_the_original_root_path() -> TestResult {
    let base = repository_local_temp_root()?;
    let container = tempfile::Builder::new()
        .prefix("norn-repository-root-replacement-")
        .tempdir_in(base)?;
    let root = container.path().join("workspace");
    fs::create_dir(&root)?;
    initialize_repository_at(&root)?;
    fs::write(root.join("tracked.txt"), b"original")?;
    git(&root, &["add", "--", "tracked.txt"])?;
    git(&root, &["commit", "-q", "-m", "original repository"])?;

    let adapter = RepositorySnapshotAdapter::discover(&root)?;
    fs::rename(&root, container.path().join("detached"))?;
    fs::create_dir(&root)?;
    initialize_repository_at(&root)?;
    fs::write(root.join("tracked.txt"), b"replacement")?;
    git(&root, &["add", "--", "tracked.txt"])?;
    git(&root, &["commit", "-q", "-m", "replacement repository"])?;

    assert!(matches!(
        adapter.acquire_current(),
        Err(SnapshotAdapterError::SnapshotChanged)
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn pinned_workspace_descriptor_outlives_a_root_rename_without_rebinding() -> TestResult {
    let base = repository_local_temp_root()?;
    let container = tempfile::Builder::new()
        .prefix("norn-workspace-descriptor-lifetime-")
        .tempdir_in(base)?;
    let root = container.path().join("workspace");
    fs::create_dir(&root)?;
    fs::write(root.join("sentinel.txt"), b"original")?;
    let workspace = WorkspaceRoot::open(fs::canonicalize(&root)?)?;

    fs::rename(&root, container.path().join("detached"))?;
    fs::create_dir(&root)?;
    fs::write(root.join("sentinel.txt"), b"replacement")?;

    let path = RepositoryPath::parse("sentinel.txt")?;
    let observed = workspace.observe(&path)?;
    let entry = observed
        .entry()
        .ok_or("pinned workspace entry is missing")?;
    assert_eq!(entry.bytes(), b"original");
    assert!(matches!(
        workspace.verify_named_identity(),
        Err(SnapshotAdapterError::SnapshotChanged)
    ));
    Ok(())
}

fn initialized_repository() -> Result<tempfile::TempDir, Box<dyn Error>> {
    let base = repository_local_temp_root()?;
    let directory = tempfile::Builder::new()
        .prefix("norn-repository-snapshot-")
        .tempdir_in(base)?;
    initialize_repository_at(directory.path())?;
    Ok(directory)
}

fn initialize_repository_at(root: &Path) -> Result<(), Box<dyn Error>> {
    git(root, &["init", "-q"])?;
    git(root, &["config", "user.name", "Synthetic Fixture"])?;
    git(root, &["config", "user.email", "fixture@example.invalid"])?;
    Ok(())
}

fn repository_with_committed_marker() -> Result<tempfile::TempDir, Box<dyn Error>> {
    let repository = initialized_repository()?;
    let policy = repository.path().join("policy");
    fs::create_dir(&policy)?;
    fs::write(policy.join("phase-lock.json"), b"{}")?;
    git(repository.path(), &["add", "--", "policy/phase-lock.json"])?;
    git(repository.path(), &["commit", "-q", "-m", "policy marker"])?;
    Ok(repository)
}

fn repository_local_temp_root() -> Result<PathBuf, Box<dyn Error>> {
    std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .ok_or_else(|| "TMPDIR must name the repository-local test scratch directory".into())
}

fn git(root: &Path, arguments: &[&str]) -> Result<(), Box<dyn Error>> {
    let permit = crate::resource::acquire_output_subprocess()?;
    let mut command = Command::new("git");
    for variable in std::env::vars_os() {
        let key = variable.0;
        if key.to_str().is_some_and(|name| name.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    let status = command
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    drop(permit);
    let status = status?;
    if !status.success() {
        return Err("synthetic Git fixture command failed".into());
    }
    Ok(())
}

fn assert_entry(
    snapshot: &norn_policy::OwnedSnapshot,
    path: &str,
    kind: EntryKind,
    bytes: &[u8],
) -> TestResult {
    let path = RepositoryPath::parse(path)?;
    let entry = snapshot
        .get(&path)
        .ok_or("expected repository entry is missing")?;
    assert_eq!(entry.kind(), kind);
    assert_eq!(entry.bytes(), bytes);
    Ok(())
}
