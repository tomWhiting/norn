//! Coordination-envelope and root-registration resolution for
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build).
//!
//! Split out of `agent/builder.rs` to keep it within the production-size
//! limit. Both helpers enforce the "no silently-ignored configuration"
//! contract: coordination is validated exactly when the agent-coordination
//! runtime is wired, and root registration is honoured only alongside it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::agent::registry::AgentRegistry;
use crate::error::{ConfigError, NornError};

fn invalid(reason: String) -> NornError {
    NornError::Config(ConfigError::InvalidConfig { reason })
}

/// Reject the zero-capacity and mutually-exclusive-session builder inputs
/// before any assembly work begins.
///
/// Every channel the builder may wire (event broadcast, inbound steering,
/// child-result, and a child's own inbound steering channel) needs a
/// non-zero capacity, and the two session-supply paths (`.session(store)`
/// and `.open_session(..)`) are mutually exclusive. These guards run first
/// in [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build) so
/// a misconfigured capacity fails loudly instead of surfacing later as a
/// closed or panicking channel.
///
/// # Errors
///
/// [`NornError::Config`] when `event_channel_capacity`, `inbound_capacity`,
/// `child_result_capacity`, or the child policy's `inbound_capacity` is
/// zero, or when both a session store and a managed session request are set.
pub(crate) fn validate_build_inputs(
    event_channel_capacity: Option<usize>,
    inbound_capacity: Option<usize>,
    session_present: bool,
    session_request_present: bool,
    child_result_capacity: Option<usize>,
    child_policy: Option<&ChildPolicy>,
) -> Result<(), NornError> {
    if event_channel_capacity == Some(0) {
        return Err(invalid(
            "event_channel_capacity is 0 — the event broadcast channel needs a \
             non-zero capacity; pick one sized to how fast consumers drain"
                .to_string(),
        ));
    }
    if inbound_capacity == Some(0) {
        return Err(invalid(
            "inbound_capacity is 0 — the inbound steering channel needs a \
             non-zero capacity"
                .to_string(),
        ));
    }
    if session_present && session_request_present {
        return Err(invalid(
            "both .session(store) and .open_session(..) are set — pass either an \
             in-memory event store or a managed persisted session, not both"
                .to_string(),
        ));
    }
    if child_result_capacity == Some(0) {
        return Err(invalid(
            "child_result_capacity is 0 — the child-result channel needs a \
             non-zero capacity"
                .to_string(),
        ));
    }
    if let Some(policy) = child_policy
        && policy.inbound_capacity == 0
    {
        return Err(invalid(
            "child_policy.inbound_capacity is 0 — a child's inbound steering \
             channel needs a non-zero capacity"
                .to_string(),
        ));
    }
    Ok(())
}

/// Compute the read-exempt roots for a confined agent (DECISIONS §0.6(b)):
/// under confinement, a confined agent may READ the well-known,
/// convention-defined skill / profile / config locations that lie OUTSIDE
/// the workspace root — the skill tool advertises companion files there, so
/// they must be readable. Write stays fully confined; the set is empty when
/// no confinement root is set; canonicalization + dedup + missing-drop
/// happen in the context setter.
///
/// The exempt set is deliberately limited to non-model-writable convention
/// locations. It is NOT seeded from `runtime_base.skill_paths`, because that
/// list includes settings-declared `skills.search_paths` merged from
/// `cwd/.norn/settings.json` / `settings.local.json` — files INSIDE the
/// confinement root that the confined agent can write. Seeding exemptions
/// from them would let an agent append an arbitrary path and have the NEXT
/// session read-exempt it: a persistent confinement escape. Settings-declared
/// `search_paths` that point outside the workspace are therefore NOT exempt
/// under confinement — that allowance belongs to the future operator
/// permission-config surface (DECISIONS §0.6(b) sketch). A skill from such a
/// path still LOADS (the skill tool reads through its own `std::fs` path in
/// `tools/skill.rs`, never through read-tool confinement), but its companion
/// files are not readable under confinement until that operator surface
/// exists.
///
/// `~/.norn/settings.json` itself is not exempted: exempt roots are directory
/// prefixes (`starts_with`), so a file-granular allowance is not expressible
/// here, and exempting its directory would be too broad (`~/.norn/` also holds
/// the active `session-store/` and legacy `sessions/` trees). Nothing extra is
/// added for it. Session transcripts for ALL workspaces and the `~/.norn/` root
/// itself are never exempted.
pub(crate) fn compute_read_exempt_roots(
    workspace_root: Option<&Path>,
    working_dir: &Path,
) -> Vec<PathBuf> {
    if let Some(root) = workspace_root {
        let mut roots: Vec<PathBuf> = Vec::new();
        // Narrow `~/.norn/` convention subdirs (NORN_HOME-aware via the
        // `config::paths` helpers) — skills, profiles, and rules only,
        // never the norn root or either session namespace.
        if let Some(skills) = crate::config::paths::skills_dir() {
            roots.push(skills);
        }
        if let Some(profiles) = crate::config::paths::profiles_dir() {
            roots.push(profiles);
        }
        if let Some(rules) = crate::config::paths::rules_dir() {
            roots.push(rules);
        }
        // Literal-home foreign-convention skill tiers, resolved exactly as
        // `build_skill_search_paths` resolves them: rooted at the real home
        // dir, because NORN_HOME moves only the norn root, not these
        // (agentskills.io / Claude Code) conventions.
        if let Some(home) = crate::config::paths::trusted_home_dir() {
            roots.push(home.join(".agents").join("skills"));
            roots.push(home.join(".claude").join("skills"));
        }
        // Home-tier profile scan dirs: only those `default_scan_dirs`
        // yields OUTSIDE the workspace. Its project-tier entries
        // (`cwd/.norn/profiles`, `cwd/.meridian/profiles`) live inside the
        // confinement root and are already readable, so they need no
        // exemption.
        for dir in crate::profile::default_scan_dirs(working_dir) {
            if !dir.starts_with(root) {
                roots.push(dir);
            }
        }
        roots
    } else {
        Vec::new()
    }
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
