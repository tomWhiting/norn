//! Session command aliases use the requested project before forwarding a resolved ID.

use super::{run_fork, run_resume};
use crate::cli::Cli;
use crate::cli::ExitCode;
use clap::Parser;
use norn::session::{CreateSessionOptions, DurabilityPolicy, SessionManager};
use std::cell::RefCell;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn aliases_scope_names_and_refuse_duplicates_before_invoking_the_agent() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = temp.path().join("store");
    let manager = SessionManager::new(&store);
    let directory = temp.path().join("selected");
    std::fs::create_dir(&directory)?;
    let options = |path: &str| CreateSessionOptions {
        model: "test-model".to_owned(),
        working_dir: path.to_owned(),
        name: Some("shared".to_owned()),
    };
    drop(manager.create(options("/other"), DurabilityPolicy::Flush)?);
    let selected = manager.create(
        options(&directory.to_string_lossy()),
        DurabilityPolicy::Flush,
    )?;
    let expected = selected.entry.id.clone();
    drop(selected);
    let parsed = || -> Result<Cli, clap::Error> {
        let mut cli = Cli::try_parse_from(["norn"])?;
        cli.working_dir = Some(directory.clone());
        Ok(cli)
    };
    let captured = RefCell::new(Vec::new());
    let agent = |cli: &Cli| {
        captured
            .borrow_mut()
            .push((cli.resume.clone(), cli.fork.clone()));
        ExitCode::Success
    };
    assert_eq!(
        run_resume(parsed()?, &store, "shared", &agent),
        ExitCode::Success
    );
    assert_eq!(
        run_fork(parsed()?, &store, "shared", &agent),
        ExitCode::Success
    );
    assert_eq!(
        *captured.borrow(),
        vec![(Some(expected.clone()), None), (None, Some(expected))]
    );
    drop(manager.create(
        options(&directory.to_string_lossy()),
        DurabilityPolicy::Flush,
    )?);
    assert_eq!(
        run_resume(parsed()?, &store, "shared", &agent),
        ExitCode::AgentError
    );
    assert_eq!(
        run_fork(parsed()?, &store, "shared", &agent),
        ExitCode::AgentError
    );
    assert_eq!(captured.borrow().len(), 2);
    Ok(())
}
