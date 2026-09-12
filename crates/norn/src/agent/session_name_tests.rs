//! Persisted agent resume/fork requests resolve names in their explicit working directory.

use super::{SessionRequest, SessionSpec};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::persistence::SessionPersistError;
use crate::session::{CreateSessionOptions, DurabilityPolicy, SessionManager};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn seed(manager: &SessionManager, directory: &str) -> Result<String, Box<dyn std::error::Error>> {
    let opened = manager.create(
        CreateSessionOptions {
            model: "test-model".to_owned(),
            working_dir: directory.to_owned(),
            name: Some("shared".to_owned()),
        },
        DurabilityPolicy::Flush,
    )?;
    opened.store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: directory.to_owned(),
    })?;
    Ok(opened.entry.id)
}

fn request(manager: &SessionManager, spec: SessionSpec) -> SessionRequest {
    SessionRequest {
        manager: manager.clone(),
        spec,
        durability: DurabilityPolicy::Flush,
    }
}

#[test]
fn disk_resume_and_fork_choose_the_requested_project_and_preserve_foreign_id_access() -> TestResult
{
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let other = seed(&manager, "/other")?;
    let current = seed(&manager, "/current")?;
    let resumed =
        request(&manager, SessionSpec::resume("shared")).open("test-model", "/current", None)?;
    assert_eq!(resumed.entry.id, current);
    drop(resumed);
    let forked = request(&manager, SessionSpec::fork("shared", None)).open(
        "test-model",
        "/current",
        None,
    )?;
    assert_ne!(forked.entry.id, current);
    assert!(forked.store.events().iter().any(
        |event| matches!(event, SessionEvent::UserMessage { content, .. } if content=="/current")
    ));
    assert!(!forked.store.events().iter().any(
        |event| matches!(event, SessionEvent::UserMessage { content, .. } if content=="/other")
    ));
    drop(forked);
    let resumed =
        request(&manager, SessionSpec::resume(&other)).open("test-model", "/current", None)?;
    assert_eq!(resumed.entry.id, other);
    Ok(())
}

#[test]
fn ambiguous_agent_requests_leave_index_and_timelines_unchanged() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let first = seed(&manager, "/current")?;
    let second = seed(&manager, "/current")?;
    let paths = [
        temp.path().join("index.jsonl"),
        temp.path().join(format!("{first}.jsonl")),
        temp.path().join(format!("{second}.jsonl")),
    ];
    let before = paths
        .iter()
        .map(std::fs::read)
        .collect::<Result<Vec<_>, _>>()?;
    for spec in [
        SessionSpec::resume("shared"),
        SessionSpec::fork("shared", None),
    ] {
        assert!(
            matches!(request(&manager, spec).open("test-model", "/current", None), Err(SessionPersistError::AmbiguousName { matches, .. }) if matches==vec![first.clone(),second.clone()])
        );
        assert_eq!(
            paths
                .iter()
                .map(std::fs::read)
                .collect::<Result<Vec<_>, _>>()?,
            before
        );
    }
    Ok(())
}
