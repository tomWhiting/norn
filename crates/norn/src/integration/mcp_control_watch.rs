//! Lifecycle-bound watchers for MCP tool-list change notifications.

use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::{Command, Envelope};
use crate::integration::McpRuntime;

pub(super) struct ToolChangeWatchers {
    sender: mpsc::WeakSender<Envelope>,
    tasks: BTreeMap<u64, JoinHandle<()>>,
}

impl ToolChangeWatchers {
    pub(super) fn new(sender: mpsc::WeakSender<Envelope>) -> Self {
        Self {
            sender,
            tasks: BTreeMap::new(),
        }
    }

    pub(super) fn reconcile(&mut self, runtime: &McpRuntime) {
        let subscriptions = runtime.tool_change_subscriptions();
        let active: BTreeSet<_> = subscriptions
            .iter()
            .map(|(_, instance_id, _)| *instance_id)
            .collect();
        self.tasks.retain(|instance_id, task| {
            if active.contains(instance_id) {
                true
            } else {
                task.abort();
                false
            }
        });
        for (name, instance_id, changes) in subscriptions {
            if self.tasks.contains_key(&instance_id) {
                continue;
            }
            let task = tokio::spawn(watch_changes(
                self.sender.clone(),
                name,
                instance_id,
                changes,
            ));
            self.tasks.insert(instance_id, task);
        }
    }

    pub(super) fn abort_all(&mut self) {
        for (_, task) in std::mem::take(&mut self.tasks) {
            task.abort();
        }
    }
}

async fn watch_changes(
    sender: mpsc::WeakSender<Envelope>,
    name: String,
    instance_id: u64,
    mut changes: watch::Receiver<u64>,
) {
    let mut handled = 0;
    loop {
        let revision = *changes.borrow_and_update();
        if revision > handled {
            if !request_refresh(&sender, name.clone(), instance_id, revision).await {
                return;
            }
            handled = revision;
        }
        if changes.changed().await.is_err() {
            return;
        }
    }
}

async fn request_refresh(
    weak: &mpsc::WeakSender<Envelope>,
    name: String,
    instance_id: u64,
    revision: u64,
) -> bool {
    let Some(sender) = weak.upgrade() else {
        return false;
    };
    let (reply, result) = oneshot::channel();
    if sender
        .send(Envelope {
            command: Command::RefreshTools {
                name,
                instance_id,
                revision,
            },
            reply,
        })
        .await
        .is_err()
    {
        return false;
    }
    match result.await {
        Ok(Ok(_response)) => true,
        Ok(Err(error)) => {
            tracing::warn!(%error, "MCP tool-list refresh failed; retaining the previous tools");
            true
        }
        Err(_closed) => false,
    }
}
