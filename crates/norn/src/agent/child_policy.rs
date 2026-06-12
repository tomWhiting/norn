//! Spawn-time child policy: the one coherent parameter a parent stamps on
//! every child it creates (Wave 3, W3.0).
//!
//! [`ChildPolicy`] bundles the messaging scope (Feature 1 — inter-agent
//! messaging), the delegation budget (Feature 2 — recursive delegation),
//! and the child's inbound-channel capacity into a single shape. The root
//! envelope is builder-set and **required** whenever the agent-coordination
//! runtime is wired
//! ([`AgentBuilder::agent_registry`](crate::agent::builder::AgentBuilder::agent_registry)):
//! building without it is a typed configuration error — Norn never assumes
//! a default policy. Spawn/fork tool calls will later narrow (never widen)
//! this envelope per child (W3.2/W3.4).
//!
//! Every type here is serde-stable (`snake_case`) because the spawn/fork
//! tools' future `child_policy` argument mirrors [`ChildPolicy`] 1:1 at
//! the JSON layer (DECISION R2).

use serde::{Deserialize, Serialize};

/// Who a child agent may message through `send_message` (DECISION M1).
///
/// Granted by the parent at spawn/fork time and enforced against registry
/// ground truth at send time (enforcement lands in W3.2). The scope is
/// always one hop: "siblings" means children of the same parent, "parent"
/// means one level up — a grandchild with [`Self::SiblingsAndParent`] may
/// message its own siblings and its direct parent, never the root.
///
/// Recommended scope for docs and examples: [`Self::SiblingsAndParent`] —
/// the audit trail and the steer/update split are the safety mechanism,
/// not isolation. This is a recommendation, not a default: the value is
/// always caller-supplied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagingScope {
    /// May message siblings under the same parent, and the parent.
    SiblingsAndParent,
    /// May message only the parent.
    ParentOnly,
    /// `send_message` is not available (tool absent from the child's
    /// surface, and refused at execute as defense-in-depth).
    None,
}

/// Delegation budget for a child's own spawning (DECISION R1).
///
/// Documented proposal for the root envelope — matching today's
/// production-proven behaviour, never assumed by the library:
/// `remaining_depth = 1` (children are leaves; deeper trees are an
/// explicit opt-in per deployment) and `max_concurrent_children = 32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationBudget {
    /// How many levels of descendants this child may create below itself.
    /// `0` = leaf (may not spawn). Decrements per level: a child spawned
    /// with `remaining_depth = n` grants its own children at most `n - 1`.
    pub remaining_depth: u32,
    /// Max non-terminal direct children this child may have at once.
    pub max_concurrent_children: usize,
}

/// The per-child policy a parent stamps on a child at spawn/fork time.
///
/// One coherent shape covering messaging scope, delegation budget, and the
/// child's inbound-channel capacity (DECISION M4 — replaces the hardcoded
/// `SPAWN_INBOUND_BUFFER` / `FORK_INBOUND_BUFFER` once W3.2/W3.4 wire it
/// through). The root envelope is set via
/// [`AgentBuilder::child_policy`](crate::agent::builder::AgentBuilder::child_policy)
/// and is required whenever the agent-coordination runtime is wired.
///
/// Documented proposal for the root envelope (a recommendation for docs
/// and examples — the library never assumes it):
///
/// ```
/// use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
///
/// let envelope = ChildPolicy {
///     messaging: MessagingScope::SiblingsAndParent,
///     delegation: DelegationBudget {
///         remaining_depth: 1,
///         max_concurrent_children: 32,
///     },
///     inbound_capacity: 32,
/// };
/// # let _ = envelope;
/// ```
///
/// **Reserved (TRACKED DEFERRAL R5, approved 2026-06-12):** a future wave
/// adds an optional per-child `loop_config` override here (children
/// currently run `AgentLoopConfig::default()` and do **not** inherit the
/// parent's `max_iterations` / `step_timeout`). Adding that `Option` field
/// is non-breaking for both the Rust type and the serde surface; until it
/// lands, spawn/fork guidance must state that children ignore the parent's
/// loop limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildPolicy {
    /// Who this child may message (Feature 1).
    pub messaging: MessagingScope,
    /// Delegation budget for this child's own spawning (Feature 2).
    pub delegation: DelegationBudget,
    /// Bounded capacity of this child's inbound channel. Must be non-zero
    /// (a zero-capacity channel cannot exist); validated at build time.
    /// Documented proposal: 32 — today's production-proven buffer.
    pub inbound_capacity: usize,
}

/// The builder-set coordination envelope, published on the agent's shared
/// [`ToolContext`](crate::tool::context::ToolContext) when the
/// agent-coordination runtime is installed.
///
/// Carries the root's [`ChildPolicy`] (the policy stamped on children when
/// a spawn call does not narrow it) and the capacity of each spawning
/// agent's child-result channel (DECISION R3 — replaces the previously
/// hardcoded capacity of 256, which is now the documented proposal, never
/// a library default). W3.2/W3.4 read this envelope at spawn/fork time;
/// in W3.0 it is carried and validated, not yet enforced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinationEnvelope {
    /// The policy the root stamps on its children when the spawn call
    /// doesn't narrow it.
    pub child_policy: ChildPolicy,
    /// Bounded capacity of a spawning agent's child-result channel.
    /// Must be non-zero; validated at build time. Documented proposal:
    /// 256 — today's production-proven buffer.
    pub child_result_capacity: usize,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Serde shape pinning: the spawn/fork tools' future `child_policy`
    /// argument mirrors this type 1:1 (DECISION R2), so the wire shape is
    /// a stable contract from the day the type lands.
    #[test]
    fn child_policy_serde_shape_is_stable() {
        let policy = ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
        };
        let json = serde_json::to_value(&policy).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({
                "messaging": "siblings_and_parent",
                "delegation": {
                    "remaining_depth": 1,
                    "max_concurrent_children": 32,
                },
                "inbound_capacity": 32,
            }),
        );
        let back: ChildPolicy = serde_json::from_value(json).expect("round-trips");
        assert_eq!(back, policy);
    }

    #[test]
    fn messaging_scope_serializes_snake_case() {
        for (scope, expected) in [
            (MessagingScope::SiblingsAndParent, "siblings_and_parent"),
            (MessagingScope::ParentOnly, "parent_only"),
            (MessagingScope::None, "none"),
        ] {
            let json = serde_json::to_value(scope).expect("serializes");
            assert_eq!(json, serde_json::json!(expected));
            let back: MessagingScope = serde_json::from_value(json).expect("round-trips");
            assert_eq!(back, scope);
        }
    }

    /// Unknown fields are rejected at the deserialization boundary — a
    /// typo'd field in a future tool argument must fail loudly, never be
    /// silently dropped.
    #[test]
    fn child_policy_rejects_unknown_fields() {
        let result: Result<ChildPolicy, _> = serde_json::from_value(serde_json::json!({
            "messaging": "parent_only",
            "delegation": { "remaining_depth": 0, "max_concurrent_children": 1 },
            "inbound_capacity": 8,
            "loop_config": {},
        }));
        let err = result.expect_err("unknown field must be rejected");
        assert!(
            err.to_string().contains("loop_config"),
            "error names the unknown field: {err}",
        );
    }

    #[test]
    fn delegation_budget_rejects_unknown_fields() {
        let result: Result<DelegationBudget, _> = serde_json::from_value(serde_json::json!({
            "remaining_depth": 1,
            "max_concurrent_children": 32,
            "max_depth": 3,
        }));
        assert!(result.is_err(), "unknown field must be rejected");
    }

    /// Missing fields fail typed — the policy has no partial form and no
    /// per-field defaults.
    #[test]
    fn child_policy_has_no_field_defaults() {
        let result: Result<ChildPolicy, _> = serde_json::from_value(serde_json::json!({
            "messaging": "none",
        }));
        assert!(result.is_err(), "missing fields must be rejected");
    }
}
