//! Thin TUI adapter for the shared live MCP command surface.

use norn::integration::{
    LiveMcpCommandError, McpControlHandle, execute_live_mcp_command, parse_live_mcp_command,
};

use crate::TuiError;

use super::slash::write_dim_line;
use super::{notices, state::AppState};

type McpCommandResult = Result<Vec<String>, LiveMcpCommandError>;
pub(super) type McpJoinResult = Result<McpCommandResult, tokio::task::JoinError>;

/// UI waiter for a command whose actor mutation is commit-on-enqueue.
///
/// Dropping this handle detaches the waiter; it does not claim to cancel an
/// operation already accepted by the serialized MCP controller.
pub(super) struct McpCommandTask {
    handle: Option<tokio::task::JoinHandle<McpCommandResult>>,
}

impl McpCommandTask {
    #[cfg(test)]
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

/// Await the installed task by mutable reference so losing a select branch
/// cannot detach its result. With no task, this future remains pending.
pub(super) async fn wait_mcp_result(task: &mut Option<McpCommandTask>) -> McpJoinResult {
    match task.as_mut().and_then(|task| task.handle.as_mut()) {
        Some(handle) => handle.await,
        None => std::future::pending().await,
    }
}

pub(super) fn render_pending_mcp_exit(state: &mut AppState) -> Result<(), TuiError> {
    write_dim_line(
        "norn: wait for the running /mcp command to finish before exiting",
        state,
    )
}

pub(super) fn handle_mcp(
    arguments: &str,
    control: Option<&McpControlHandle>,
    task: &mut Option<McpCommandTask>,
    state: &mut AppState,
) -> Result<(), TuiError> {
    match start_mcp(arguments, control, task) {
        Ok(McpStartOutcome::Started) => write_dim_line("MCP command running...", state),
        Ok(McpStartOutcome::Busy) => {
            write_dim_line("norn: another /mcp command is still running", state)
        }
        Err(error) => {
            notices::error(state, "/mcp failed", &error.to_string())?;
            Ok(())
        }
    }
}

/// Consume exactly one completed waiter and retain all command diagnostics.
pub(super) fn render_completed_mcp(
    state: &mut AppState,
    task: &mut Option<McpCommandTask>,
    result: McpJoinResult,
) -> Result<(), TuiError> {
    *task = None;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, "TUI MCP command task failed");
            notices::error(state, "/mcp command task failed", &error.to_string())?;
            return Ok(());
        }
    };
    match result {
        Ok(lines) => {
            for line in lines {
                write_dim_line(&line, state)?;
            }
        }
        Err(error) => {
            notices::error(state, "/mcp failed", &error.to_string())?;
        }
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
    async fn losing_completion_select_preserves_the_installed_waiter()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::future::Future;
        use std::task::Poll;

        let release = Arc::new(Notify::new());
        let worker_release = Arc::clone(&release);
        let mut task = Some(McpCommandTask {
            handle: Some(tokio::spawn(async move {
                worker_release.notified().await;
                Ok(vec!["original result".to_owned()])
            })),
        });
        {
            let mut waiting = std::pin::pin!(wait_mcp_result(&mut task));
            std::future::poll_fn(|context| match waiting.as_mut().poll(context) {
                Poll::Pending => Poll::Ready(Ok(())),
                Poll::Ready(result) => {
                    Poll::Ready(Err(format!("wait completed before release: {result:?}")))
                }
            })
            .await?;
        }
        assert!(mcp_exit_is_blocked(task.as_ref()));
        release.notify_one();
        let result = wait_mcp_result(&mut task).await??;
        assert_eq!(result, ["original result"]);
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
