use std::sync::Arc;
use std::time::Duration;
use std::{error::Error, io};

use uuid::Uuid;

use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentRegistry;
use crate::integration::rhai::context::NornRhaiContext;
use crate::provider::mock::MockProvider;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::tool::registry::ToolRegistry;

pub(super) type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub(super) fn require<T>(value: Option<T>, message: &'static str) -> Result<T, io::Error> {
    value.ok_or(io::Error::other(message))
}

pub(super) fn require_error<T, E>(
    result: Result<T, E>,
    message: &'static str,
) -> Result<E, io::Error> {
    match result {
        Ok(_) => Err(io::Error::other(message)),
        Err(error) => Ok(error),
    }
}

pub(super) fn build_context() -> NornRhaiContext {
    build_context_with_provider(Arc::new(MockProvider::new(Vec::new())))
}

pub(super) fn build_context_with_provider(provider: Arc<dyn Provider>) -> NornRhaiContext {
    NornRhaiContext {
        registry: AgentRegistry::shared(),
        router: Arc::new(MessageRouter::new()),
        provider,
        agent_id: Uuid::new_v4(),
        runtime: tokio::runtime::Handle::current(),
        event_store: Arc::new(EventStore::new()),
        tool_registry: Some(Arc::new(ToolRegistry::new())),
        working_dir: crate::tool::context::SharedWorkingDir::default(),
        child_policy: crate::agent::child_policy::ChildPolicy {
            messaging: crate::agent::child_policy::MessagingScope::SiblingsAndParent,
            delegation: crate::agent::child_policy::DelegationBudget {
                remaining_depth: 2,
                max_concurrent_children: 8,
            },
            inbound_capacity: 8,
            loop_config: None,
        },
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
        events: None,
    }
}

pub(super) async fn wait_for_terminal(
    registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    child_id: Uuid,
) -> Result<(), io::Error> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if registry
            .read()
            .get(child_id)
            .is_some_and(|entry| entry.status.is_terminal())
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "script child never reached a terminal status",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
