//! Coordination-envelope and root-registration resolution for
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! Split out of `agent/builder.rs` to keep it within the production-size
//! limit. Both helpers enforce the "no silently-ignored configuration"
//! contract: coordination is validated exactly when the agent-coordination
//! runtime is wired, and root registration is honoured only alongside it.

use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::agent::registry::AgentRegistry;
use crate::error::{ConfigError, NornError};

fn invalid(reason: String) -> NornError {
    NornError::Config(ConfigError::InvalidConfig { reason })
}

/// The validated agent-coordination inputs: the shared registry and the
/// coordination envelope, present exactly when `.agent_registry(..)` was
/// wired.
pub(crate) type Coordination = (Arc<RwLock<AgentRegistry>>, CoordinationEnvelope);

/// Resolve the coordination envelope from the builder's three coordination
/// inputs.
///
/// The envelope is required exactly when the agent-coordination runtime is
/// wired (`.agent_registry(..)` makes `spawn_agent` / `fork` functional and
/// creates the child-result channel), and rejected when it could only be
/// silently ignored. Norn never assumes a default child policy or channel
/// capacity.
///
/// # Errors
///
/// [`NornError::Config`] when `.agent_registry(..)` is set without the full
/// envelope ([`ChildPolicy`] and the child-result capacity both required),
/// or when either envelope half is set without `.agent_registry(..)` (it
/// would be silently ignored).
pub(crate) fn resolve_coordination(
    agent_registry: Option<Arc<RwLock<AgentRegistry>>>,
    child_policy: Option<ChildPolicy>,
    child_result_capacity: Option<usize>,
) -> Result<Option<Coordination>, NornError> {
    match (agent_registry, child_policy, child_result_capacity) {
        (Some(agent_registry), Some(child_policy), Some(child_result_capacity)) => Ok(Some((
            agent_registry,
            CoordinationEnvelope {
                child_policy,
                child_result_capacity,
            },
        ))),
        (Some(_), child_policy, child_result_capacity) => {
            let mut missing = Vec::new();
            if child_policy.is_none() {
                missing.push(".child_policy(ChildPolicy { .. })");
            }
            if child_result_capacity.is_none() {
                missing.push(".child_result_capacity(<n>)");
            }
            Err(invalid(format!(
                "agent coordination is wired (.agent_registry(..)) but the \
                 coordination envelope is incomplete — set {} on the builder; \
                 Norn never assumes a default child policy or channel capacity \
                 (recommended starting envelope: MessagingScope::SiblingsAndParent, \
                 remaining_depth 1, max_concurrent_children 32, \
                 inbound_capacity 32, child_result_capacity 256)",
                missing.join(" and "),
            )))
        }
        (None, None, None) => Ok(None),
        (None, child_policy, child_result_capacity) => {
            let mut orphaned = Vec::new();
            if child_policy.is_some() {
                orphaned.push("child_policy");
            }
            if child_result_capacity.is_some() {
                orphaned.push("child_result_capacity");
            }
            Err(invalid(format!(
                "{} set but agent coordination is not wired — the value would \
                 be silently ignored; add .agent_registry(..) or remove the \
                 coordination envelope",
                orphaned.join(" and "),
            )))
        }
    }
}

/// Resolve the agent's id, honouring `.register_root(..)` (D2).
///
/// Root registration is opt-in and effective only alongside coordination:
/// the reservation mints the id, so the registered root entry and the
/// running agent share one id. Set without coordination the root entry
/// would be silently unregistered, so that combination fails loudly.
/// Without `.register_root(..)`, the explicit `.agent_id(..)` is used, or a
/// fresh id is minted.
///
/// # Errors
///
/// [`NornError::Config`] when `.register_root(..)` is set without
/// coordination, and any registry-reservation error surfaced by
/// [`AgentRegistry::reserve`] / [`SpawnGuard::confirm`](crate::agent::registry::SpawnGuard::confirm).
pub(crate) fn resolve_root_agent_id(
    register_root: Option<(String, String)>,
    coordination: Option<&Coordination>,
    model: &str,
    explicit_agent_id: Option<Uuid>,
) -> Result<Uuid, NornError> {
    match (register_root, coordination) {
        (Some((path, role)), Some((agent_registry, envelope))) => {
            let guard = AgentRegistry::reserve(
                agent_registry,
                path,
                role,
                model.to_owned(),
                None,
                envelope.child_policy.clone(),
                None,
            )?;
            let id = guard.id();
            guard.confirm()?;
            Ok(id)
        }
        (Some(_), None) => Err(invalid(
            "register_root is set but agent coordination is not wired — the \
             root entry would be silently unregistered; add .agent_registry(..) \
             or remove .register_root(..)"
                .to_string(),
        )),
        (None, _) => Ok(explicit_agent_id.unwrap_or_else(Uuid::new_v4)),
    }
}
