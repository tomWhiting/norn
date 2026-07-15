//! `norn policy` repository-policy commands.

use std::io::{self, Write as _};

use norn::repository_snapshot::RepositorySnapshotAdapter;
use norn_policy::{PolicyState, evaluate_p1};

use crate::cli::{ExitCode, PolicyCmd, PolicyOutputFormat};

/// Evaluate the complete repository through the shared production adapter.
pub fn run_policy(command: PolicyCmd) -> ExitCode {
    match command {
        PolicyCmd::Check { format } => run_check(format),
    }
}

fn run_check(format: PolicyOutputFormat) -> ExitCode {
    let Ok(start) = std::env::current_dir() else {
        eprintln!("repository policy failed: current directory is unavailable");
        return ExitCode::AgentError;
    };
    let Ok(adapter) = RepositorySnapshotAdapter::discover(&start) else {
        eprintln!("repository policy failed: repository acquisition is unavailable");
        return ExitCode::AgentError;
    };
    let Ok(acquired) = adapter.acquire_p1() else {
        eprintln!("repository policy failed: complete repository acquisition failed");
        return ExitCode::AgentError;
    };
    let state = evaluate_p1(acquired.evaluation_input());
    if adapter.revalidate_current(acquired.current()).is_err() {
        eprintln!("repository policy failed: repository changed during evaluation");
        return ExitCode::AgentError;
    }
    if write_state(format, &state).is_err() {
        eprintln!("repository policy failed: result output could not be written");
        return ExitCode::AgentError;
    }
    match state {
        PolicyState::Ready(report) if report.is_clear() => ExitCode::Success,
        PolicyState::Absent | PolicyState::Invalid(_) | PolicyState::Ready(_) => {
            ExitCode::AgentError
        }
    }
}

fn write_state(format: PolicyOutputFormat, state: &PolicyState) -> Result<(), io::Error> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    match format {
        PolicyOutputFormat::Json => {
            serde_json::to_writer(&mut output, state).map_err(io::Error::other)?;
            output.write_all(b"\n")?;
        }
    }
    output.flush()
}
