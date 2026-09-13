//! An actual registered child timeline for PTY inspection, with no child provider execution.

use std::sync::Arc;

use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use norn::agent::registry::AgentRegistry;
use norn::session::action_log::ActionLog;
use norn::session::action_log_tree::ActionLogTree;
use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};
use norn::tool::ToolRegistry;
use norn::tool::context::ToolContext;
use uuid::Uuid;

use super::{TestResult, Workspace};

pub(super) const AGENTS_ENV: &str = "NORN_RETAINED_AGENT_WORKSPACE";

pub(super) fn executor(
    enabled: bool,
    root: Uuid,
    registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    store: &Arc<EventStore>,
    binding: &SessionBinding,
) -> TestResult<Arc<ToolRegistry>> {
    if !enabled {
        return Ok(Arc::new(ToolRegistry::new()));
    }
    let reservation = AgentRegistry::reserve(
        registry,
        "/root/inspectable-child".to_owned(),
        "reader".to_owned(),
        "gpt-5.5".to_owned(),
        Some(root),
        ChildPolicy {
            messaging: MessagingScope::ParentOnly,
            delegation: DelegationBudget {
                remaining_depth: 0,
                max_concurrent_children: 1,
            },
            inbound_capacity: 1,
            loop_config: None,
        },
        None,
    )?;
    let child = reservation.id();
    reservation.confirm()?;
    store.bind_view_source(binding, root, None)?;
    let child_store = Arc::new(EventStore::new());
    child_store.bind_view_source(&SessionBinding::ephemeral_root(), child, Some(root))?;
    child_store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "Recorded child conversation from its own store".to_owned(),
    })?;
    let tree = Arc::new(ActionLogTree::new(root));
    tree.register(root, None, Arc::new(ActionLog::new(Arc::clone(store))));
    tree.register(child, Some(root), Arc::new(ActionLog::new(child_store)));
    let context = Arc::new(ToolContext::empty());
    context.insert_extension(tree);
    Ok(Arc::new(ToolRegistry::with_context(context)))
}

/// Exercise the original App and PTY teardown with one recorded descendant.
pub fn with_agents(exercise: impl FnOnce(&mut Workspace) -> TestResult) -> TestResult {
    super::with_launch(None, false, false, true, None, |app| {
        exercise(app)?;
        Ok("workspace fixture prompt".to_owned())
    })
}
