//! Low-limit regressions for descriptor retention across idle objects.

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::process::{ProcessManager, Spool};
use crate::resource::descriptor_snapshot;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::{DurabilityPolicy, JsonlSink, PersistenceSink};

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
    let governor = crate::resource::DescriptorGovernor::global()?;
    let baseline = governor.available();
    let manager = Arc::new(ProcessManager::new(Some("fd-active".to_owned()), None));

    let missing = home.join("missing-working-directory");
    let failure = manager.spawn("printf never", &missing, None).await;
    assert!(
        failure.is_err(),
        "missing working directory must fail spawn"
    );
    wait_for_available(&governor, baseline).await?;

    let mut handles = Vec::with_capacity(20);
    let mut denied = 0usize;
    for _ in 0..20 {
        match manager.spawn("printf x; sleep 0.1", &home, None).await {
            Ok(handle) => handles.push(handle),
            Err(crate::process::ProcessError::DescriptorAdmission(_)) => denied += 1,
            Err(error) => return Err(error.into()),
        }
    }
    assert!(!handles.is_empty(), "low-limit child must admit some work");
    assert!(denied > 0, "low-limit child must reach typed admission");
    wait_for_available(&governor, baseline).await?;
    assert_eq!(manager.list().len(), handles.len());
    assert!(handles.iter().all(|handle| !handle.is_running()));

    let killed = manager.spawn("sleep 30", &home, None).await?;
    wait_for_available(&governor, baseline.saturating_sub(3)).await?;
    let _status = killed.kill().await;
    wait_for_available(&governor, baseline).await?;
    manager.shutdown();
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
        Some("sessions" | "spools" | "processes" | "active_processes") | None => Ok(value),
        Some(other) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown descriptor-retention child case: {other}"),
        )
        .into()),
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

async fn wait_for_available(
    governor: &crate::resource::DescriptorGovernor,
    expected: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if governor.available() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|elapsed| {
        io::Error::other(format!(
            "descriptor capacity did not return to {expected}; observed {}: {elapsed}",
            governor.available(),
        ))
    })?;
    Ok(())
}
