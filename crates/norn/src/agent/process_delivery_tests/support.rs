//! Shared fixtures for process-delivery behavioral tests.

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use super::super::*;
use crate::agent::PendingMailboxLease;
use crate::session::SessionBinding;

pub(super) fn completion(label: &str, exit_code: Option<i32>, killed: bool) -> ProcessCompletion {
    ProcessCompletion {
        process_label: label.to_owned(),
        command: "cargo test".to_owned(),
        exit_code,
        killed,
        started_at: Utc::now(),
        exited_at: Utc::now(),
        spool_path: "~/.norn/outputs/sess/processes/p1.log".to_owned(),
    }
}

pub(super) struct RegisteredDelivery {
    sink: ProcessMessageDelivery,
    _mailbox_lease: Arc<PendingMailboxLease>,
}

impl std::ops::Deref for RegisteredDelivery {
    type Target = ProcessMessageDelivery;

    fn deref(&self) -> &Self::Target {
        &self.sink
    }
}

impl ProcessNotifier for RegisteredDelivery {
    fn deliver_completion(&self, completion: ProcessCompletion) {
        self.sink.deliver_completion(completion);
    }

    fn deliver_watch_alert(&self, alert: WatchAlert) {
        self.sink.deliver_watch_alert(alert);
    }
}

pub(super) fn delivery_with_runtime(
    agent_id: Uuid,
    inbound: Option<InboundSender>,
    pending: Arc<PendingAgentMessages>,
    event_store: Arc<EventStore>,
    registry: Option<Arc<RwLock<AgentRegistry>>>,
    wake_registry: Option<Arc<AgentWakeRegistry>>,
) -> RegisteredDelivery {
    let mailbox_lease = Arc::new(PendingMailboxLease::new());
    pending
        .register_root_mailbox(
            agent_id,
            SessionBinding::ephemeral_root().mailbox_id(),
            &event_store,
            &mailbox_lease,
        )
        .expect("register process-delivery test mailbox");
    RegisteredDelivery {
        sink: ProcessMessageDelivery {
            agent_id,
            inbound,
            pending: Some(pending),
            event_store,
            registry,
            wake_registry,
        },
        _mailbox_lease: mailbox_lease,
    }
}

pub(super) fn delivery(
    agent_id: Uuid,
    inbound: Option<InboundSender>,
    pending: Arc<PendingAgentMessages>,
    event_store: Arc<EventStore>,
) -> RegisteredDelivery {
    delivery_with_runtime(agent_id, inbound, pending, event_store, None, None)
}

pub(super) struct HomeGuard {
    prior: Option<std::ffi::OsString>,
}

impl HomeGuard {
    pub(super) fn set(path: &std::path::Path) -> Self {
        let prior = std::env::var_os("NORN_HOME");
        // SAFETY: paired with `#[serial]`; no concurrent reader.
        unsafe { std::env::set_var("NORN_HOME", path) };
        Self { prior }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => unsafe { std::env::set_var("NORN_HOME", value) },
            None => unsafe { std::env::remove_var("NORN_HOME") },
        }
    }
}

pub(super) fn match_alert(watch_id: &str, process_id: &str) -> WatchAlert {
    WatchAlert {
        watch_id: watch_id.to_owned(),
        process_id: process_id.to_owned(),
        brief: "watch for errors".to_owned(),
        spool_start: 10,
        spool_end: 42,
        kind: WatchAlertKind::Match {
            excerpt: "ERROR: boom\n".to_owned(),
            matched_at: Utc::now(),
        },
    }
}
