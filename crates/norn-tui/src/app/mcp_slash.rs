//! Thin TUI adapter for the shared live MCP command surface.

use norn::integration::{
    LiveMcpCommandError, McpControlHandle, execute_live_mcp_command, parse_live_mcp_command,
};

use crate::TuiError;
use crate::terminal::setup::TerminalGuard;

use super::slash::write_dim_line;

type McpCommandResult = Result<Vec<String>, LiveMcpCommandError>;
type McpJoinResult = Result<McpCommandResult, tokio::task::JoinError>;

/// UI waiter for a command whose actor mutation is commit-on-enqueue.
///
/// Dropping this handle detaches the waiter; it does not claim to cancel an
/// operation already accepted by the serialized MCP controller.
pub(super) struct McpCommandTask {
    handle: Option<tokio::task::JoinHandle<McpCommandResult>>,
}

impl McpCommandTask {
    pub(super) fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }

    async fn complete(mut self) -> Option<McpJoinResult> {
        let handle = self.handle.take()?;
        Some(handle.await)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum McpStartOutcome {
    Started,
    Busy,
}

fn start_mcp(
    arguments: &str,
    control: Option<&McpControlHandle>,
    task: &mut Option<McpCommandTask>,
) -> Result<McpStartOutcome, LiveMcpCommandError> {
    if task.is_some() {
        return Ok(McpStartOutcome::Busy);
    }
    let command = parse_live_mcp_command(arguments)?;
    let control = control.cloned();
    *task = Some(McpCommandTask {
        handle: Some(tokio::spawn(async move {
            execute_live_mcp_command(control.as_ref(), command).await
        })),
    });
    Ok(McpStartOutcome::Started)
}

pub(super) const fn mcp_exit_is_blocked(task: Option<&McpCommandTask>) -> bool {
    task.is_some()
}

pub(super) fn render_pending_mcp_exit(guard: &mut TerminalGuard) -> Result<(), TuiError> {
    write_dim_line(
        "norn: wait for the running /mcp command to finish before exiting",
        guard,
    )
}

pub(super) fn handle_mcp(
    arguments: &str,
    control: Option<&McpControlHandle>,
    task: &mut Option<McpCommandTask>,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    match start_mcp(arguments, control, task) {
        Ok(McpStartOutcome::Started) => write_dim_line("MCP command running...", guard),
        Ok(McpStartOutcome::Busy) => {
            write_dim_line("norn: another /mcp command is still running", guard)
        }
        Err(error) => write_dim_line(&format!("norn: {error}"), guard),
    }
}

pub(super) async fn render_completed_mcp(
    task: &mut Option<McpCommandTask>,
    guard: &mut TerminalGuard,
) -> Result<(), TuiError> {
    if !task.as_ref().is_some_and(McpCommandTask::is_finished) {
        return Ok(());
    }
    let Some(task) = task.take() else {
        return Ok(());
    };
    let result = match task.complete().await {
        Some(Ok(result)) => result,
        Some(Err(error)) => {
            tracing::error!(%error, "TUI MCP command task failed");
            return write_dim_line("norn: the /mcp command task failed", guard);
        }
        None => return Ok(()),
    };
    match result {
        Ok(lines) => {
            for line in lines {
                write_dim_line(&line, guard)?;
            }
        }
        Err(error) => write_dim_line(&format!("norn: {error}"), guard)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use tokio::sync::Notify;

    use super::*;
    use norn::config::{McpApprovalStore, McpConfigState};
    use norn::integration::{
        McpActivationCandidate, McpActivationRequest, McpCandidateBuilder, McpCandidateError,
        McpRuntime, McpRuntimeStore,
    };
    use norn::tool::{ToolContext, ToolGeneration, ToolGenerationStore, ToolRegistry};

    struct BlockingBuilder {
        started: Notify,
        release: Notify,
    }

    #[async_trait]
    impl McpCandidateBuilder for BlockingBuilder {
        async fn build(
            &self,
            request: McpActivationRequest,
        ) -> Result<McpActivationCandidate, McpCandidateError> {
            self.started.notify_one();
            self.release.notified().await;
            let registry = ToolRegistry::with_context(request.previous().context());
            let generation = Arc::new(ToolGeneration::from_registry(&registry, request.revision()));
            Ok(McpActivationCandidate::new(
                generation,
                request.previous_runtime(),
            ))
        }
    }

    #[tokio::test]
    async fn dispatch_stays_responsive_rejects_overlap_and_blocks_exit_until_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let state = McpConfigState::load(project.path(), BTreeMap::new())?;
        let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
        let generations = Arc::new(ToolGenerationStore::new(Arc::new(
            ToolGeneration::from_registry(&registry, 0),
        )));
        let runtimes = Arc::new(McpRuntimeStore::new(
            generations.snapshot(),
            Arc::new(McpRuntime::empty()),
        ));
        let builder = Arc::new(BlockingBuilder {
            started: Notify::new(),
            release: Notify::new(),
        });
        let control = McpControlHandle::spawn(
            state,
            McpApprovalStore::at_root(home.path())?,
            Arc::clone(&builder) as Arc<dyn McpCandidateBuilder>,
            generations,
            runtimes,
        )?;
        let started = builder.started.notified();
        let mut task = None;

        assert_eq!(
            start_mcp("add docs stdio fixture", Some(&control), &mut task)?,
            McpStartOutcome::Started
        );
        started.await;
        assert!(mcp_exit_is_blocked(task.as_ref()));
        assert_eq!(
            start_mcp("list", Some(&control), &mut task)?,
            McpStartOutcome::Busy
        );

        builder.release.notify_one();
        let completion = task
            .take()
            .ok_or("MCP task was not installed")?
            .complete()
            .await
            .ok_or("MCP task handle was missing")??;
        assert!(completion.is_ok());
        assert!(!mcp_exit_is_blocked(task.as_ref()));
        Ok(())
    }

    #[tokio::test]
    async fn dropped_ui_waiter_does_not_cancel_an_enqueued_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let state = McpConfigState::load(project.path(), BTreeMap::new())?;
        let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
        let generations = Arc::new(ToolGenerationStore::new(Arc::new(
            ToolGeneration::from_registry(&registry, 0),
        )));
        let runtimes = Arc::new(McpRuntimeStore::new(
            generations.snapshot(),
            Arc::new(McpRuntime::empty()),
        ));
        let builder = Arc::new(BlockingBuilder {
            started: Notify::new(),
            release: Notify::new(),
        });
        let control = McpControlHandle::spawn(
            state,
            McpApprovalStore::at_root(home.path())?,
            Arc::clone(&builder) as Arc<dyn McpCandidateBuilder>,
            generations,
            runtimes,
        )?;
        let started = builder.started.notified();
        let mut task = None;

        assert_eq!(
            start_mcp("add docs stdio fixture", Some(&control), &mut task)?,
            McpStartOutcome::Started
        );
        started.await;
        drop(task.take());
        builder.release.notify_one();

        let statuses = control.list().await?;
        assert!(statuses.iter().any(|status| status.name == "docs"));
        Ok(())
    }
}
