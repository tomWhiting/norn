//! Low-limit regressions for descriptor retention across idle objects.

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::process::{ProcessManager, Spool};
use crate::resource::{PRIVATE_FS_OPERATION_PEAK, descriptor_snapshot};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::persistence::index::{
    DeleteCheckpoint, delete_session_transaction_with_hook, publish_new_child_session,
    publish_new_session, read_index,
};
use crate::session::{
    DurabilityPolicy, JsonlSink, PersistenceSink, ResumeFidelity, SESSION_FORMAT_VERSION,
    SessionIndexEntry, SessionRecordOrigin, SessionStatus,
};

const CHILD_CASE_ENV: &str = "NORN_FD_RETENTION_CASE";
const CHILD_HOME_ENV: &str = "NORN_FD_RETENTION_HOME";
const LOW_NOFILE_LIMIT: u64 = 48;

#[tokio::test]
async fn retained_idle_session_sinks_stay_bounded() -> Result<(), Box<dyn std::error::Error>> {
    const NAME: &str = "tests::descriptor_retention::retained_idle_session_sinks_stay_bounded";
    if child_case()?.as_deref() != Some("sessions") {
        return run_child(NAME, "sessions");
    }
    lower_nofile_limit()?;
    let home = child_home()?;
    let baseline = open_count()?;
    let mut sinks = Vec::with_capacity(128);
    for id in 0..128 {
        sinks.push(JsonlSink::open_with(
            &home.join(format!("sessions/{id}.jsonl")),
            DurabilityPolicy::Flush,
        )?);
    }
    assert_eq!(sinks.len(), 128);
    assert_bounded_growth(baseline, open_count()?, 2)
}

#[tokio::test]
async fn retained_idle_process_spools_stay_bounded() -> Result<(), Box<dyn std::error::Error>> {
    const NAME: &str = "tests::descriptor_retention::retained_idle_process_spools_stay_bounded";
    if child_case()?.as_deref() != Some("spools") {
        return run_child(NAME, "spools");
    }
    lower_nofile_limit()?;
    let home = child_home()?;
    let baseline = open_count()?;
    let mut spools = Vec::with_capacity(128);
    for id in 0..128 {
        spools.push(Spool::create(home.join(format!("spools/{id}.log"))).await?);
    }
    assert_eq!(spools.len(), 128);
    assert_bounded_growth(baseline, open_count()?, 2)
}

#[tokio::test]
async fn completed_process_registry_stays_bounded() -> Result<(), Box<dyn std::error::Error>> {
    const NAME: &str = "tests::descriptor_retention::completed_process_registry_stays_bounded";
    if child_case()?.as_deref() != Some("processes") {
        return run_child(NAME, "processes");
    }
    lower_nofile_limit()?;
    let home = child_home()?;
    let baseline = open_count()?;
    let manager = Arc::new(ProcessManager::new(Some("fd-retention".to_owned()), None));
    for _ in 0..200 {
        let handle = manager.spawn("printf x", &home, None).await?;
        let mut exited = handle.exit_receiver();
        if !*exited.borrow() {
            tokio::time::timeout(Duration::from_secs(5), exited.changed())
                .await
                .map_err(io::Error::other)?
                .map_err(io::Error::other)?;
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(manager.list().len(), 200);
    assert_bounded_growth(baseline, open_count()?, 6)
}

#[tokio::test]
async fn active_process_permits_release_on_terminal_paths() -> Result<(), Box<dyn std::error::Error>>
{
    const NAME: &str =
        "tests::descriptor_retention::active_process_permits_release_on_terminal_paths";
    if child_case()?.as_deref() != Some("active_processes") {
        return run_child(NAME, "active_processes");
    }
    lower_nofile_limit()?;
    let home = child_home()?;
    // Account for the single fixture descriptor before fixing the governor's
    // baseline. No per-child parent descriptor is retained by the release gate.
    let (release_path, mut release) = active_process_release_gate(&home)?;
    let governor = crate::resource::DescriptorGovernor::global()?;
    let baseline = governor.available();
    let manager = Arc::new(ProcessManager::new(Some("fd-active".to_owned()), None));
    let manager_guard = crate::process::ProcessManagerGuard::new(Arc::clone(&manager));

    let missing = home.join("missing-working-directory");
    let failure = manager.spawn("printf never", &missing, None).await;
    assert!(
        failure.is_err(),
        "missing working directory must fail spawn"
    );
    active_process_permits_returned(&governor, baseline, "failed working-directory spawn").await?;

    let environment = crate::tool::context::ProcessEnv::new([(
        "NORN_TEST_ACTIVE_PROCESS_RELEASE",
        release_path.as_os_str(),
    )]);
    let command = r#"exec python3 -I -S -c '
import os, sys
with open(os.environ["NORN_TEST_ACTIVE_PROCESS_RELEASE"], "rb", buffering=0) as gate:
    if gate.read(1) != b"R":
        raise RuntimeError("active-process fixture did not receive its release byte")
sys.stdout.write("x")
'"#;
    let mut handles = Vec::with_capacity(20);
    let mut denied = 0usize;
    for attempt in 0..20 {
        match manager.spawn(command, &home, Some(&environment)).await {
            Ok(handle) => handles.push(handle),
            Err(crate::process::ProcessError::DescriptorAdmission(_)) => denied += 1,
            Err(error) => {
                return Err(io::Error::other(format!(
                    "held-process admission attempt {attempt} using {} failed: {error}",
                    release_path.display(),
                ))
                .into());
            }
        }
    }
    assert!(!handles.is_empty(), "low-limit child must admit some work");
    assert!(
        handles
            .iter()
            .all(crate::process::ProcessHandle::is_running),
        "every admitted child remains held before release",
    );
    assert!(denied > 0, "low-limit child must reach typed admission");
    // Each unbuffered child consumes exactly one byte. Pending child startup
    // cannot lose a release: the parent keeps both FIFO endpoints open.
    std::io::Write::write_all(&mut release, &vec![b'R'; handles.len()]).map_err(|error| {
        io::Error::other(format!(
            "releasing {} held processes through {}: {error}",
            handles.len(),
            release_path.display(),
        ))
    })?;
    active_processes_exited(&handles).await?;
    active_process_permits_returned(&governor, baseline, "naturally released children").await?;
    assert_eq!(manager.list().len(), handles.len());
    assert!(handles.iter().all(|handle| !handle.is_running()));
    for handle in &handles {
        let (output, committed) = handle.spool().read_from(0).await?;
        assert_eq!(
            output,
            b"out x\n",
            "released child {} ran its workload",
            handle.label()
        );
        assert_eq!(committed, 6);
    }

    // No bytes remain in the shared gate after every admitted child exits.
    // This new child can leave only through the explicit kill path.
    let killed = manager.spawn(command, &home, Some(&environment)).await?;
    let held = baseline.checked_sub(3).ok_or_else(|| {
        io::Error::other(format!(
            "descriptor baseline {baseline} cannot retain three process permits"
        ))
    })?;
    assert_eq!(governor.available(), held);
    assert!(
        killed.is_running(),
        "the kill fixture remains held at its gate"
    );
    assert_eq!(killed.kill().await, crate::process::ProcessStatus::Killed);
    active_process_permits_returned(&governor, baseline, "explicitly killed child").await?;
    drop(manager_guard);
    Ok(())
}

/// Observe natural child exit through its retained exit watch.
async fn active_processes_exited(handles: &[crate::process::ProcessHandle]) -> io::Result<()> {
    for handle in handles {
        let mut exited = handle.exit_receiver();
        tokio::time::timeout(Duration::from_secs(10), exited.wait_for(|done| *done))
            .await
            .map_err(|elapsed| {
                io::Error::other(format!(
                    "released process {} did not exit within the fixture deadline; status {:?}: {elapsed}",
                    handle.label(),
                    handle.status(),
                ))
            })?
            .map_err(|error| {
                io::Error::other(format!(
                    "waiting for released process {}: {error}",
                    handle.label(),
                ))
            })?;
        assert_eq!(
            handle.status(),
            crate::process::ProcessStatus::Exited { code: 0 }
        );
    }
    Ok(())
}

/// One fixed descriptor holds a shared release FIFO for this low-limit case.
fn active_process_release_gate(home: &std::path::Path) -> io::Result<(PathBuf, std::fs::File)> {
    let path = home.join("active-process-release.fifo");
    // rustix's mkfifoat is unavailable on Apple; the existing Unix fixture
    // platforms provide the POSIX utility without adding unsafe test code.
    let created = Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(&path)
        .output()
        .map_err(|error| {
            io::Error::other(format!("creating release FIFO {}: {error}", path.display()))
        })?;
    if !created.status.success() {
        return Err(io::Error::other(format!(
            "creating release FIFO {} failed with {}: {}",
            path.display(),
            created.status,
            String::from_utf8_lossy(&created.stderr),
        )));
    }
    let descriptor = rustix::fs::openat(
        rustix::fs::CWD,
        &path,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        io::Error::other(format!("opening release FIFO {}: {error}", path.display()))
    })?;
    Ok((path, std::fs::File::from(descriptor)))
}

/// Acquiring the whole original budget proves return through semaphore push.
async fn active_process_permits_returned(
    governor: &crate::resource::DescriptorGovernor,
    baseline: usize,
    stage: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Retain the original helper's ten-second fixture deadline; the wait
    // itself is still driven by permit release, without periodic inspection.
    let returned = tokio::time::timeout(
        Duration::from_secs(10),
        governor.acquire(u32::try_from(baseline)?),
    )
    .await
    .map_err(|elapsed| {
        io::Error::other(format!(
            "descriptor capacity did not return to {baseline} after {stage}; observed {}: {elapsed}",
            governor.available(),
        ))
    })?
    .map_err(|error| {
        io::Error::other(format!(
            "reclaiming {baseline} process permits after {stage}: {error}"
        ))
    })?;
    assert_eq!(governor.available(), 0);
    drop(returned);
    assert_eq!(governor.available(), baseline);
    Ok(())
}

#[test]
fn descendant_deletion_stays_within_private_fs_peak() -> Result<(), Box<dyn std::error::Error>> {
    const NAME: &str =
        "tests::descriptor_retention::descendant_deletion_stays_within_private_fs_peak";
    if child_case()?.as_deref() != Some("session_deletion") {
        return run_child(NAME, "session_deletion");
    }
    lower_nofile_limit()?;
    let data_dir = child_home()?.join("session-store");
    let root = deletion_entry("low-fd-root", None, None);
    let target = deletion_entry(
        "low-fd-target",
        Some(format!("{}/children/target.jsonl", root.id)),
        Some(root.id.clone()),
    );
    publish_new_session(&data_dir, &root, &[], None)?;
    publish_new_child_session(&data_dir, &target, &[], root.generation, None)?;
    for ordinal in 0..64 {
        let descendant = deletion_entry(
            &format!("low-fd-descendant-{ordinal}"),
            Some(format!("{}/children/descendant-{ordinal}.jsonl", root.id)),
            Some(target.id.clone()),
        );
        publish_new_child_session(&data_dir, &descendant, &[], target.generation, None)?;
    }

    let governor = crate::resource::DescriptorGovernor::global()?;
    let baseline = governor.available();
    let operation_peak = usize::try_from(PRIVATE_FS_OPERATION_PEAK)?;
    let reserve_weight = baseline.checked_sub(operation_peak).ok_or_else(|| {
        io::Error::other(format!(
            "descriptor budget {baseline} cannot retain the private-fs peak {operation_peak}"
        ))
    })?;
    let reserve = governor.try_acquire(u32::try_from(reserve_weight)?)?;
    let mut checkpoints = 0;
    let mut observe = |_: DeleteCheckpoint| {
        checkpoints += 1;
        if governor.available() != 0 {
            return Err(io::Error::other(format!(
                "deletion retained {} unclaimed descriptor permits",
                governor.available()
            ))
            .into());
        }
        Ok(())
    };
    delete_session_transaction_with_hook(&data_dir, &target.id, None, &mut observe)?;
    assert_eq!(checkpoints, 2);
    assert_eq!(governor.available(), operation_peak);
    assert_eq!(read_index(&data_dir)?, vec![root]);
    drop(reserve);
    assert_eq!(governor.available(), baseline);
    Ok(())
}

#[tokio::test]
async fn lazy_spool_reopen_rejects_replaced_inode() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("spool.log");
    let displaced = temporary.path().join("original.log");
    let spool = Spool::create(path.clone()).await?;
    spool.append_raw(b"original").await?;
    std::fs::rename(&path, displaced)?;
    std::fs::write(&path, b"replacement")?;

    let error = spool
        .append_raw(b"-followed")
        .await
        .err()
        .ok_or_else(|| io::Error::other("lazy spool reopen accepted a replacement inode"))?;
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read(&path)?, b"replacement");
    Ok(())
}

#[test]
fn lazy_session_reopen_rejects_replaced_inode() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("session.jsonl");
    let displaced = temporary.path().join("original.jsonl");
    let mut sink = JsonlSink::open(&path)?;
    std::fs::rename(&path, displaced)?;
    std::fs::write(&path, b"replacement")?;
    let event = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "must not follow".to_owned(),
    };

    let error = sink
        .persist(&event)
        .err()
        .ok_or_else(|| io::Error::other("lazy session reopen accepted a replacement inode"))?;
    assert!(error.to_string().contains("changed identity"));
    assert_eq!(std::fs::read(&path)?, b"replacement");
    Ok(())
}

fn run_child(test_name: &str, case: &str) -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD_CASE_ENV, case)
        .env(CHILD_HOME_ENV, home.path())
        .env("NORN_HOME", home.path())
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "low-NOFILE child failed for {case} with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    ))
    .into())
}

fn child_case() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let value = std::env::var(CHILD_CASE_ENV).ok();
    match value.as_deref() {
        Some("sessions" | "spools" | "processes" | "active_processes" | "session_deletion")
        | None => Ok(value),
        Some(other) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown descriptor-retention child case: {other}"),
        )
        .into()),
    }
}

fn deletion_entry(
    id: &str,
    rel_path: Option<String>,
    parent_id: Option<String>,
) -> SessionIndexEntry {
    let now = chrono::Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "gpt-test".to_owned(),
        working_dir: "/workspace".to_owned(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path,
        parent_id,
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
        provider_state_identity: None,
    }
}

fn child_home() -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::env::var_os(CHILD_HOME_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor-retention child home is missing",
            )
            .into()
        })
}

fn lower_nofile_limit() -> io::Result<()> {
    let inherited = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    let target = inherited
        .maximum
        .map_or(LOW_NOFILE_LIMIT, |hard| hard.min(LOW_NOFILE_LIMIT));
    if target < 32 {
        return Err(io::Error::other(format!(
            "inherited hard NOFILE limit {target} is too low for the regression harness"
        )));
    }
    rustix::process::setrlimit(
        rustix::process::Resource::Nofile,
        rustix::process::Rlimit {
            current: Some(target),
            maximum: inherited.maximum,
        },
    )
    .map_err(io::Error::from)
}

fn open_count() -> io::Result<u64> {
    descriptor_snapshot()
        .open
        .map(|open| open.count)
        .ok_or_else(|| io::Error::other("open-descriptor count is unavailable"))
}

fn assert_bounded_growth(
    baseline: u64,
    observed: u64,
    allowance: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    if observed <= baseline.saturating_add(allowance) {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "retained objects grew open descriptors from {baseline} to {observed}; allowance {allowance}"
    ))
    .into())
}
