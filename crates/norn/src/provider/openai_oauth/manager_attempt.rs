//! Structural completion signaling for one shared refresh attempt.

use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::{AuthManager, RefreshTokenError};

type RefreshResult = Result<(), RefreshTokenError>;

/// Shared observer for one owned refresh worker.
pub(super) struct RefreshAttempt {
    outcome: watch::Receiver<Option<RefreshResult>>,
}

impl RefreshAttempt {
    pub(super) fn new() -> (Arc<Self>, RefreshAttemptCompletion) {
        let (sender, outcome) = watch::channel(None);
        (
            Arc::new(Self { outcome }),
            RefreshAttemptCompletion {
                sender: Some(sender),
            },
        )
    }

    pub(super) async fn wait(&self) -> RefreshResult {
        let mut outcome = self.outcome.clone();
        loop {
            let recorded = outcome.borrow_and_update().clone();
            if let Some(result) = recorded {
                return result;
            }
            if outcome.changed().await.is_err() {
                return Err(task_terminated_error());
            }
        }
    }

    pub(super) fn is_terminal(&self) -> bool {
        self.outcome.borrow().is_some() || self.outcome.has_changed().is_err()
    }
}

impl AuthManager {
    pub(super) async fn join_registered_attempt(&self) -> RefreshResult {
        let Some(attempt) = self.refresh_attempt.lock().await.clone() else {
            return Ok(());
        };
        let result = attempt.wait().await;
        self.clear_attempt_if_current(&attempt).await;
        result
    }

    pub(super) async fn clear_attempt_if_current(&self, attempt: &Arc<RefreshAttempt>) {
        let mut registered = self.refresh_attempt.lock().await;
        if registered
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, attempt))
        {
            *registered = None;
        }
    }
}

impl std::fmt::Debug for RefreshAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RefreshAttempt")
            .field("terminal", &self.is_terminal())
            .finish()
    }
}

/// Single owner of the attempt's terminal publication channel.
pub(super) struct RefreshAttemptCompletion {
    sender: Option<watch::Sender<Option<RefreshResult>>>,
}

impl RefreshAttemptCompletion {
    fn finish(mut self, result: RefreshResult) {
        if let Some(sender) = self.sender.take()
            && sender.send(Some(result)).is_err()
        {
            tracing::debug!("OAuth refresh attempt completed without live observers");
        }
    }
}

/// Observe the owned worker and publish exactly one terminal outcome.
pub(super) async fn supervise_refresh_worker(
    worker: JoinHandle<RefreshResult>,
    completion: RefreshAttemptCompletion,
) {
    let result = match worker.await {
        Ok(result) => result,
        Err(join_error) => {
            tracing::error!(
                task_cancelled = join_error.is_cancelled(),
                task_panicked = join_error.is_panic(),
                "OAuth refresh worker terminated without an outcome",
            );
            Err(task_terminated_error())
        }
    };
    completion.finish(result);
}

fn task_terminated_error() -> RefreshTokenError {
    RefreshTokenError::Indeterminate(
        "OAuth refresh worker terminated without recording an authority outcome".to_owned(),
    )
}
