//! `RunMonitored` — delegate watching of a long-running task to a
//! lightweight model.
//!
//! The parent agent receives a [`MonitorHandle`] for querying progress
//! without consuming the task's full output. The monitor loop is intended
//! to summarise raw progress events using a cheap model (e.g. Haiku); v1
//! ships the scaffolding (config, watch channel, lifecycle) and leaves
//! the LLM-driven summarisation pluggable through the provided
//! [`Provider`](crate::provider::traits::Provider) handle.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::provider::traits::Provider;

/// Configuration for a monitored task run.
#[derive(Clone, Debug)]
pub struct MonitorConfig {
    /// Human-readable description of the work being monitored.
    pub task_description: String,
    /// Model identifier for the lightweight monitor loop.
    pub monitor_model: String,
    /// How often the monitor loop publishes a heartbeat update.
    pub poll_interval: Duration,
}

/// Latest snapshot reported by the monitor loop.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MonitorStatus {
    /// Short summary of progress so far.
    pub summary: String,
    /// Optional progress percentage in `[0, 100]`.
    pub progress_pct: Option<f32>,
    /// Set when the underlying task has finished.
    pub is_complete: bool,
    /// Wall-clock timestamp of the last update.
    pub last_updated: DateTime<Utc>,
}

/// Handle returned to the parent for inspecting monitor progress.
pub struct MonitorHandle {
    rx: watch::Receiver<MonitorStatus>,
}

impl MonitorHandle {
    /// Return the latest known status.
    #[must_use]
    pub fn query(&self) -> MonitorStatus {
        self.rx.borrow().clone()
    }

    /// Await `is_complete == true` and return the final status.
    ///
    /// If the underlying sender is dropped before completion, the current
    /// status snapshot is returned.
    pub async fn wait_complete(&mut self) -> MonitorStatus {
        loop {
            if self.rx.borrow().is_complete {
                return self.rx.borrow().clone();
            }
            if self.rx.changed().await.is_err() {
                return self.rx.borrow().clone();
            }
        }
    }
}

/// Spawn `task_future` under a lightweight monitor.
///
/// Two tasks are spawned:
///
/// * A *task task* driving `task_future` to completion. When it finishes
///   it pushes a final `is_complete = true` status.
/// * A *monitor task* heartbeating every `config.poll_interval` until
///   the task task completes. The monitor task is the natural hook for
///   wiring an LLM summariser (see `provider`); v1 publishes a textual
///   heartbeat to keep the scaffolding observable while leaving the
///   LLM-driven summary path open.
///
/// Returns the [`JoinHandle`] for the task task and a [`MonitorHandle`]
/// over the heartbeat channel.
#[must_use]
pub fn run_monitored<F, T>(
    config: &MonitorConfig,
    task_future: F,
    _provider: Arc<dyn Provider>,
) -> (JoinHandle<T>, MonitorHandle)
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let initial = MonitorStatus {
        summary: format!("monitoring: {}", config.task_description),
        progress_pct: None,
        is_complete: false,
        last_updated: Utc::now(),
    };
    let (tx, rx) = watch::channel(initial);

    let monitor_tx = tx.clone();
    let poll_interval = config.poll_interval;
    let task_description = config.task_description.clone();
    let monitor_model = config.monitor_model.clone();

    let monitor_handle = tokio::spawn(async move {
        if poll_interval.is_zero() {
            // No heartbeats requested; the task is woken only on completion.
            std::future::pending::<()>().await;
        } else {
            let mut tick = tokio::time::interval(poll_interval);
            // First tick fires immediately — skip it so we report after the
            // first interval elapses.
            tick.tick().await;
            loop {
                tick.tick().await;
                let update = MonitorStatus {
                    summary: format!("[{monitor_model}] heartbeat: {task_description}"),
                    progress_pct: None,
                    is_complete: false,
                    last_updated: Utc::now(),
                };
                if monitor_tx.send(update).is_err() {
                    break;
                }
            }
        }
    });

    let join = tokio::spawn(async move {
        let result = task_future.await;
        // Stop the heartbeat task and wait for it to actually finish BEFORE
        // publishing the terminal status. `abort` only takes effect at an
        // await point, so a heartbeat already past its `tick().await` could
        // otherwise publish `is_complete: false` *after* the terminal status,
        // overwriting it in the watch channel — and since the monitor task is
        // then dead, `wait_complete` would hang on a status nobody updates
        // again.
        monitor_handle.abort();
        if let Err(err) = monitor_handle.await
            && !err.is_cancelled()
        {
            tracing::warn!(error = %err, "monitor heartbeat task terminated abnormally");
        }
        if tx
            .send(MonitorStatus {
                summary: "task completed".to_string(),
                progress_pct: Some(100.0),
                is_complete: true,
                last_updated: Utc::now(),
            })
            .is_err()
        {
            tracing::debug!("monitor status receiver dropped before terminal status was published");
        }
        result
    });

    (join, MonitorHandle { rx })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::provider::mock::MockProvider;

    #[tokio::test]
    async fn wait_complete_returns_after_task_finishes() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let config = MonitorConfig {
            task_description: "demo".to_string(),
            monitor_model: "haiku".to_string(),
            poll_interval: Duration::from_millis(50),
        };
        let task = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            42_i32
        };

        let (join, mut handle) = run_monitored(&config, task, provider);

        let status = handle.wait_complete().await;
        assert!(status.is_complete);
        assert_eq!(status.progress_pct, Some(100.0));

        let result = join.await.expect("join task");
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn query_reflects_initial_status() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let config = MonitorConfig {
            task_description: "demo".to_string(),
            monitor_model: "haiku".to_string(),
            poll_interval: Duration::from_secs(60),
        };
        let task = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
        };

        let (join, handle) = run_monitored(&config, task, provider);
        let initial = handle.query();
        assert!(!initial.is_complete);
        assert!(initial.summary.contains("monitoring"));
        join.await.expect("join");
    }

    #[tokio::test]
    async fn heartbeat_publishes_intermediate_status() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        let config = MonitorConfig {
            task_description: "long".to_string(),
            monitor_model: "haiku".to_string(),
            poll_interval: Duration::from_millis(10),
        };
        let task = async {
            tokio::time::sleep(Duration::from_millis(60)).await;
        };

        let (join, mut handle) = run_monitored(&config, task, provider);

        // Wait for at least one heartbeat by polling the watch channel.
        let mut saw_heartbeat = false;
        for _ in 0..20 {
            if handle.rx.changed().await.is_err() {
                break;
            }
            let snap = handle.query();
            if !snap.is_complete && snap.summary.contains("heartbeat") {
                saw_heartbeat = true;
                break;
            }
            if snap.is_complete {
                break;
            }
        }

        let final_status = handle.wait_complete().await;
        assert!(final_status.is_complete);
        assert!(saw_heartbeat, "expected at least one heartbeat update");

        join.await.expect("join");
    }

    /// Regression for the terminal-status race: with an aggressive heartbeat
    /// and a task that finishes mid-heartbeat, a heartbeat publish landing
    /// after the terminal publish used to overwrite `is_complete: true` in
    /// the watch channel, permanently losing completion. The ordering fix
    /// (abort + join the monitor before publishing) makes the terminal
    /// status the channel's last word — always.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_status_never_overwritten_by_racing_heartbeat() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        // Many iterations of a deliberately racy configuration: 1ms
        // heartbeats against a ~1ms task, on a multi-threaded runtime so the
        // heartbeat and the completion publish genuinely interleave.
        for round in 0..200 {
            let config = MonitorConfig {
                task_description: format!("racy-{round}"),
                monitor_model: "haiku".to_string(),
                poll_interval: Duration::from_millis(1),
            };
            let task = async {
                tokio::time::sleep(Duration::from_millis(1)).await;
            };
            let (join, mut handle) = run_monitored(&config, task, Arc::clone(&provider));
            join.await.expect("task joins");

            // The task (and therefore the terminal publish) has fully
            // finished. Whatever the watch channel holds now is final — no
            // heartbeat may follow. A lost terminal status shows up here as
            // is_complete == false.
            let status = handle.query();
            assert!(
                status.is_complete,
                "round {round}: terminal is_complete was overwritten by a racing heartbeat",
            );
            // And the reactive path agrees without hanging.
            let waited = tokio::time::timeout(Duration::from_secs(1), handle.wait_complete())
                .await
                .expect("wait_complete must not hang on a lost terminal status");
            assert!(waited.is_complete);
        }
    }
}
