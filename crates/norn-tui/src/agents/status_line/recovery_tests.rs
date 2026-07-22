use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use norn::agent::PendingAgentMessages;
use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use norn::agent::registry::AgentRegistry;
use parking_lot::RwLock;
use uuid::Uuid;

use super::AgentStatusPanel;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn confirm_agent(
    registry: &Arc<RwLock<AgentRegistry>>,
    path: &str,
    parent_id: Option<Uuid>,
    remaining_depth: u32,
) -> TestResult<Uuid> {
    let guard = AgentRegistry::reserve(
        registry,
        path.to_owned(),
        "worker".to_owned(),
        "claude".to_owned(),
        parent_id,
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        },
        None,
    )?;
    let id = guard.id();
    guard.confirm()?;
    Ok(id)
}

#[test]
fn public_snapshot_keeps_terminal_recovery_visible_and_registered() -> TestResult {
    let registry = AgentRegistry::shared();
    let root = confirm_agent(&registry, "/root", None, 5)?;
    let child_id = confirm_agent(&registry, "/root/recovery", Some(root), 4)?;
    let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
    panel.set_terminal_recovery_probe(Arc::new(move |id| id == child_id));

    let t0 = Instant::now();
    panel.snapshot(t0);
    registry.write().mark_failed(child_id)?;
    panel.snapshot(t0);

    let expired = t0 + Duration::from_millis(3_100);
    let (view, _) = panel.snapshot(expired);
    assert!(
        view.visible.iter().any(|entry| entry.id == child_id),
        "unresolved terminal recovery must remain visible after hold expiry"
    );
    assert!(
        registry.read().get(child_id).is_some(),
        "unresolved terminal recovery must retain the registry entry"
    );
    assert!(registry.read().tombstone(child_id).is_none());

    panel.set_pending_messages(None);
    let (view, _) = panel.snapshot(expired);
    assert!(
        !view.visible.iter().any(|entry| entry.id == child_id),
        "the expired entry can disappear once recovery is discharged"
    );
    assert!(registry.read().get(child_id).is_none());
    assert!(registry.read().tombstone(child_id).is_some());
    Ok(())
}

#[test]
fn set_pending_messages_retains_and_releases_the_runtime_store() {
    let registry = AgentRegistry::shared();
    let pending_messages = Arc::new(PendingAgentMessages::new());
    let mut panel = AgentStatusPanel::new(registry);
    assert_eq!(Arc::strong_count(&pending_messages), 1);

    panel.set_pending_messages(Some(Arc::clone(&pending_messages)));
    assert_eq!(Arc::strong_count(&pending_messages), 2);
    assert!(panel.terminal_recovery_probe.is_some());

    panel.set_pending_messages(None);
    assert_eq!(Arc::strong_count(&pending_messages), 1);
    assert!(panel.terminal_recovery_probe.is_none());
}
