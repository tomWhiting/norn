//! Spawn-time child policy: the one coherent parameter a parent stamps on
//! every child it creates (Wave 3, W3.0).
//!
//! [`ChildPolicy`] bundles the messaging scope (Feature 1 — inter-agent
//! messaging), the delegation budget (Feature 2 — recursive delegation),
//! the child's inbound-channel capacity, and the optional per-child
//! loop-shaping overrides ([`ChildLoopConfig`], R5) into a single shape.
//! The root
//! envelope is builder-set and **required** whenever the agent-coordination
//! runtime is wired
//! ([`AgentBuilder::agent_registry`](crate::agent::builder::AgentBuilder::agent_registry)):
//! building without it is a typed configuration error — Norn never assumes
//! a default policy. The envelope's `child_policy` is the **root agent's
//! own** granted policy; every child's grant is computed from its
//! spawner's grant by [`ChildPolicy::grant_for_child`] — inherited with
//! the delegation depth decremented one level, or narrowed (never
//! widened) by the spawn/fork tools' per-spawn `child_policy` argument
//! (W3.4, DECISION R2).
//!
//! Every type here is serde-stable (`snake_case`) because the spawn/fork
//! tools' per-spawn `child_policy` narrowing argument mirrors
//! [`ChildPolicy`] 1:1 at the JSON layer (DECISION R2).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::r#loop::linger::LingerPolicy;
use crate::r#loop::runner::AgentLoopConfig;

/// A per-spawn `child_policy` argument that would exceed the spawning
/// agent's own granted [`ChildPolicy`] — narrowing violations, refused
/// typed and honest, always naming the caller's own budget (Wave 3
/// §"One coherent spawn-time policy parameter").
///
/// One implementation serves every enforcement point: the spawn/fork
/// tools validate the model-supplied argument with it, and
/// [`AgentRegistry::reserve`](crate::agent::registry::AgentRegistry::reserve)
/// re-validates the stamped grant against registry ground truth as
/// defense-in-depth, so a widened grant is unrepresentable no matter
/// which path tried to mint it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyNarrowingError {
    /// The spawning agent's own granted `remaining_depth` is 0 — it is a
    /// leaf and may not create children at all.
    #[error(
        "delegation depth exhausted: this agent's granted delegation budget has \
         remaining_depth = 0 (leaf) — it may not spawn or fork children"
    )]
    DepthExhausted,
    /// `child_policy.delegation.remaining_depth` is not strictly below the
    /// spawning agent's own remaining depth.
    #[error(
        "child_policy.delegation.remaining_depth = {requested} exceeds this agent's \
         budget: its own granted remaining_depth is {granted}, so a child may be \
         granted at most {}",
        granted.saturating_sub(1)
    )]
    DepthExceeded {
        /// The depth requested for the child.
        requested: u32,
        /// The spawning agent's own granted remaining depth (≥ 1 — a
        /// zero-depth spawner fails as [`Self::DepthExhausted`] first).
        granted: u32,
    },
    /// `child_policy.delegation.max_concurrent_children` exceeds the
    /// spawning agent's own granted concurrency budget.
    #[error(
        "child_policy.delegation.max_concurrent_children = {requested} exceeds this \
         agent's own granted budget of {granted}"
    )]
    ConcurrencyExceeded {
        /// The concurrency cap requested for the child.
        requested: usize,
        /// The spawning agent's own granted cap.
        granted: usize,
    },
    /// `child_policy.messaging` is wider than the spawning agent's own
    /// granted scope.
    #[error(
        "child_policy.messaging = \"{}\" widens this agent's own granted scope \
         \"{}\" — a parent may tighten its children's scope, never widen it",
        requested.as_str(),
        granted.as_str()
    )]
    ScopeWidened {
        /// The scope requested for the child.
        requested: MessagingScope,
        /// The spawning agent's own granted scope.
        granted: MessagingScope,
    },
    /// `child_policy.inbound_capacity` exceeds the spawning agent's own
    /// granted inbound capacity.
    #[error(
        "child_policy.inbound_capacity = {requested} exceeds this agent's own \
         granted capacity of {granted}"
    )]
    InboundCapacityExceeded {
        /// The inbound capacity requested for the child.
        requested: usize,
        /// The spawning agent's own granted capacity.
        granted: usize,
    },
    /// `child_policy.inbound_capacity` is 0 — a zero-capacity inbound
    /// channel cannot exist.
    #[error(
        "child_policy.inbound_capacity is 0 — a child's inbound steering channel \
         needs a non-zero buffer"
    )]
    ZeroInboundCapacity,
}

/// Who a child agent may message through `signal_agent` (DECISION M1).
///
/// Granted by the parent at spawn/fork time and enforced against registry
/// ground truth at send time. The scope is
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
    /// `signal_agent` is not available (tool absent from the child's
    /// surface, and refused at execute as defense-in-depth).
    None,
}

impl MessagingScope {
    /// The scope's stable wire label (the serde `snake_case` tag), used
    /// in honest failure messages so the model reads the same string it
    /// would pass in a `child_policy` argument.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SiblingsAndParent => "siblings_and_parent",
            Self::ParentOnly => "parent_only",
            Self::None => "none",
        }
    }

    /// Containment rank for narrowing checks: the recipient sets are
    /// strictly nested — `None` ⊂ `ParentOnly` ⊂ `SiblingsAndParent` —
    /// so "may a parent grant scope X when it holds scope Y" reduces to
    /// `rank(X) <= rank(Y)`.
    fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::ParentOnly => 1,
            Self::SiblingsAndParent => 2,
        }
    }

    /// True when `self` grants no more than `granted` — i.e. `self` is a
    /// valid narrowing of `granted`.
    #[must_use]
    pub fn is_within(self, granted: Self) -> bool {
        self.rank() <= granted.rank()
    }
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

/// Per-child loop-shaping overrides — the model-suppliable, serde-stable
/// SUBSET of [`AgentLoopConfig`] a parent may grant a child (R5 closure,
/// 2026-06-13).
///
/// This is deliberately **not** the full [`AgentLoopConfig`]: that struct
/// carries harness-only concerns (`schema_tool_name`, `cache_key`,
/// compaction thresholds, conversation-state mode, `output_schema`) that a
/// model-supplied per-spawn argument must never control. The subset is
/// exactly the execution-shaping knobs:
///
/// - [`Self::max_iterations`] — provider round-trip cap per step,
/// - [`Self::step_timeout_secs`] — wall-clock cap per step,
/// - [`Self::linger_secs`] — the deadline for an opt-in
///   [`LingerPolicy`], letting a **mid-tree** agent wait at its would-stop
///   boundaries for its own children's late results (before R5 only the
///   root could linger, so late grandchild results orphaned at the child
///   level).
///
/// Every field is optional; an unset field defers to
/// [`AgentLoopConfig::default()`] for that knob — exactly the
/// pre-R5 behavior, not an invented default (children have always run
/// `AgentLoopConfig::default()`).
///
/// **No narrowing rules** (deliberate, R5): [`ChildPolicy::grant_for_child`]
/// passes this through unchanged on inherit-with-decrement, and a
/// per-spawn `child_policy` argument may set any value regardless of the
/// spawner's own. Loop config is execution shaping, not authority: the
/// security surface is the delegation budget (depth/children, strictly
/// narrowed) and the messaging scope (strictly narrowed). Iteration and
/// time caps only shape how a child spends the spawner's own subtree
/// budget — and children already ran unconstrained
/// `AgentLoopConfig::default()` before R5, so an unconstrained override
/// widens nothing.
///
/// Durations are integers in **seconds**, matching the codebase's
/// model-facing duration convention (the `bash` tool's `timeout` and the
/// web `fetch` tool's `timeout` are both integer seconds); the unit is in
/// the field name so the wire shape is self-describing.
///
/// Interaction note: a child granted both `max_iterations` and
/// `linger_secs` can drain a late child result at a would-stop boundary
/// (the linger's purpose) and then immediately stop at the top-of-loop
/// iteration check without acting on it — typed, honest, and
/// usage-complete (the drained subtree still rolls up), but budget the
/// cap with at least one iteration of headroom when the child must
/// *respond* to what it lingered for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildLoopConfig {
    /// Optional hard cap on provider round-trips within one of the
    /// child's steps. Unset → the library default (uncapped).
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// Optional wall-clock cap, in seconds, on each of the child's
    /// steps. Unset → the library default (uncapped).
    #[serde(default)]
    pub step_timeout_secs: Option<u64>,
    /// Optional linger deadline, in seconds: when set, the child's loop
    /// waits at its would-stop boundaries (up to this long per boundary)
    /// for late inbound messages and its own children's results instead
    /// of returning immediately — see [`LingerPolicy`]. Unset → the
    /// child returns the moment its model would stop (the historical
    /// behavior; its children's late results are then undeliverable).
    #[serde(default)]
    pub linger_secs: Option<u64>,
}

impl ChildLoopConfig {
    /// Build the child's effective [`AgentLoopConfig`]: this granted
    /// subset applied onto [`AgentLoopConfig::default()`]. Every unset
    /// field keeps the library default — byte-for-byte what a child ran
    /// before R5.
    #[must_use]
    pub fn to_loop_config(self) -> AgentLoopConfig {
        let mut config = AgentLoopConfig::default();
        if let Some(max_iterations) = self.max_iterations {
            config.max_iterations = Some(max_iterations);
        }
        if let Some(secs) = self.step_timeout_secs {
            config.step_timeout = Some(Duration::from_secs(secs));
        }
        if let Some(secs) = self.linger_secs {
            config.linger = Some(LingerPolicy {
                deadline: Duration::from_secs(secs),
            });
        }
        config
    }

    /// Resolve a granted optional override into the child's effective
    /// loop config: `None` → [`AgentLoopConfig::default()`] exactly (the
    /// status quo for every pre-R5 grant), `Some` →
    /// [`Self::to_loop_config`]. The single assembly point every child
    /// launch path (spawn, fork, rhai) uses.
    #[must_use]
    pub fn resolve(granted: Option<Self>) -> AgentLoopConfig {
        granted.map_or_else(AgentLoopConfig::default, Self::to_loop_config)
    }
}

/// The per-child policy a parent stamps on a child at spawn/fork time.
///
/// One coherent shape covering messaging scope, delegation budget, and the
/// child's inbound-channel capacity (DECISION M4 — since W3.2, child
/// inbound channels are sized from `inbound_capacity`; the hardcoded
/// spawn/fork buffer constants it replaced are deleted). The root envelope
/// is set via
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
///     loop_config: None,
/// };
/// # let _ = envelope;
/// ```
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
    /// Optional per-child loop-shaping overrides (R5 closure): when this
    /// grant's `loop_config` is set, the child's launch wrapper builds
    /// its [`AgentLoopConfig`] by applying the subset onto
    /// [`AgentLoopConfig::default()`] — see [`ChildLoopConfig::resolve`].
    /// `None` means the child runs `AgentLoopConfig::default()`
    /// byte-for-byte, exactly the pre-R5 behavior; absent in JSON
    /// deserializes to `None`, so envelopes and per-spawn arguments
    /// written before R5 keep their meaning unchanged.
    ///
    /// Not subject to narrowing — see [`ChildLoopConfig`] for the
    /// execution-shaping-not-authority rationale.
    #[serde(default)]
    pub loop_config: Option<ChildLoopConfig>,
}

impl ChildPolicy {
    /// Compute the [`ChildPolicy`] this agent grants a child it is about
    /// to spawn or fork, where `self` is the spawning agent's **own**
    /// granted policy (the builder envelope's `child_policy` for a root;
    /// the harness-stamped grant for everyone deeper).
    ///
    /// `requested` is the spawn/fork tool's optional per-spawn
    /// `child_policy` argument (DECISION R2 — mirrors this type 1:1 at
    /// the JSON layer):
    ///
    /// - **`None` (the default derivation):** inherit-with-decrement —
    ///   the child receives the spawner's own policy with
    ///   `delegation.remaining_depth` reduced by exactly one level, per
    ///   [`DelegationBudget::remaining_depth`]'s decrement-per-level
    ///   contract. Messaging scope, concurrency cap, inbound capacity,
    ///   and `loop_config` are inherited unchanged.
    /// - **`Some` (narrowing only):** the request is validated against
    ///   the spawner's own grant — depth strictly decremented
    ///   (`requested.remaining_depth ≤ self.remaining_depth - 1`),
    ///   concurrency / inbound capacity at most the spawner's own, scope
    ///   contained in the spawner's own. A parent can tighten, never
    ///   widen. `loop_config` is exempt from narrowing and taken
    ///   verbatim from the request (R5): it is execution shaping, not
    ///   authority — see [`ChildLoopConfig`].
    ///
    /// # Errors
    ///
    /// [`PolicyNarrowingError::DepthExhausted`] when `self` has
    /// `remaining_depth == 0` (the spawner is a leaf — no grant exists,
    /// requested or derived); otherwise the specific widening violation,
    /// each naming the caller's own budget.
    pub fn grant_for_child(&self, requested: Option<Self>) -> Result<Self, PolicyNarrowingError> {
        let own = self.delegation;
        if own.remaining_depth == 0 {
            return Err(PolicyNarrowingError::DepthExhausted);
        }
        let Some(requested) = requested else {
            return Ok(Self {
                messaging: self.messaging,
                delegation: DelegationBudget {
                    remaining_depth: own.remaining_depth - 1,
                    max_concurrent_children: own.max_concurrent_children,
                },
                inbound_capacity: self.inbound_capacity,
                loop_config: self.loop_config,
            });
        };
        if requested.delegation.remaining_depth > own.remaining_depth - 1 {
            return Err(PolicyNarrowingError::DepthExceeded {
                requested: requested.delegation.remaining_depth,
                granted: own.remaining_depth,
            });
        }
        if requested.delegation.max_concurrent_children > own.max_concurrent_children {
            return Err(PolicyNarrowingError::ConcurrencyExceeded {
                requested: requested.delegation.max_concurrent_children,
                granted: own.max_concurrent_children,
            });
        }
        if !requested.messaging.is_within(self.messaging) {
            return Err(PolicyNarrowingError::ScopeWidened {
                requested: requested.messaging,
                granted: self.messaging,
            });
        }
        if requested.inbound_capacity == 0 {
            return Err(PolicyNarrowingError::ZeroInboundCapacity);
        }
        if requested.inbound_capacity > self.inbound_capacity {
            return Err(PolicyNarrowingError::InboundCapacityExceeded {
                requested: requested.inbound_capacity,
                granted: self.inbound_capacity,
            });
        }
        Ok(requested)
    }
}

/// The builder-set coordination envelope, published on the agent's shared
/// [`ToolContext`](crate::tool::context::ToolContext) when the
/// agent-coordination runtime is installed.
///
/// Carries the root agent's **own** granted [`ChildPolicy`] — the budget
/// its spawns/forks are checked against, and the base every child grant
/// is derived from via [`ChildPolicy::grant_for_child`] — and the
/// capacity of each spawning agent's child-result channel (DECISION R3 —
/// replaces the previously hardcoded capacity of 256, which is now the
/// documented proposal, never a library default). The same
/// `child_result_capacity` sizes the per-agent child-result channel at
/// every depth, so result delivery cannot drift across the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinationEnvelope {
    /// The root agent's **own** granted policy — the budget its spawns
    /// are charged against. Children receive a derived grant (depth
    /// decremented one level; further narrowed by a per-spawn
    /// `child_policy` arg), never this policy verbatim: a root granted
    /// `remaining_depth = 1` spawns leaf children.
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
            loop_config: None,
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
                "loop_config": null,
            }),
        );
        let back: ChildPolicy = serde_json::from_value(json).expect("round-trips");
        assert_eq!(back, policy);
    }

    /// Serde shape pinning for [`ChildLoopConfig`] (R5): it is both
    /// model-suppliable (per-spawn `child_policy.loop_config`) and
    /// envelope-carried, so the wire shape is a stable contract in both
    /// directions — durations as integer seconds, explicit-unit field
    /// names.
    #[test]
    fn child_loop_config_serde_shape_is_stable() {
        let full = ChildLoopConfig {
            max_iterations: Some(12),
            step_timeout_secs: Some(300),
            linger_secs: Some(45),
        };
        let json = serde_json::to_value(full).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({
                "max_iterations": 12,
                "step_timeout_secs": 300,
                "linger_secs": 45,
            }),
        );
        let back: ChildLoopConfig = serde_json::from_value(json).expect("round-trips");
        assert_eq!(back, full);

        // Every field is independently optional: an empty object is the
        // all-unset config (= library defaults), and partial objects
        // deserialize with the remaining fields unset.
        let empty: ChildLoopConfig = serde_json::from_value(serde_json::json!({})).expect("empty");
        assert_eq!(
            empty,
            ChildLoopConfig {
                max_iterations: None,
                step_timeout_secs: None,
                linger_secs: None,
            },
        );
        let partial: ChildLoopConfig =
            serde_json::from_value(serde_json::json!({ "linger_secs": 10 })).expect("partial");
        assert_eq!(partial.linger_secs, Some(10));
        assert_eq!(partial.max_iterations, None);
        assert_eq!(partial.step_timeout_secs, None);
    }

    /// A `child_policy` JSON written before R5 (no `loop_config` key)
    /// deserializes with `loop_config: None` — existing envelopes and
    /// per-spawn arguments keep their meaning unchanged.
    #[test]
    fn child_policy_without_loop_config_deserializes_to_none() {
        let policy: ChildPolicy = serde_json::from_value(serde_json::json!({
            "messaging": "parent_only",
            "delegation": { "remaining_depth": 0, "max_concurrent_children": 1 },
            "inbound_capacity": 8,
        }))
        .expect("pre-R5 shape still deserializes");
        assert_eq!(policy.loop_config, None);
    }

    /// Typos *inside* `loop_config` are rejected at the deserialization
    /// boundary, exactly like typos at the `child_policy` level — a
    /// misspelled knob must fail loudly, never silently leave the child
    /// on library defaults where the caller intended a cap.
    #[test]
    fn child_loop_config_rejects_unknown_fields() {
        let result: Result<ChildLoopConfig, _> = serde_json::from_value(serde_json::json!({
            "max_iterations": 3,
            "linger_seconds": 10,
        }));
        let err = result.expect_err("unknown field must be rejected");
        assert!(
            err.to_string().contains("linger_seconds"),
            "error names the unknown field: {err}",
        );
    }

    /// `loop_config: None` resolves to `AgentLoopConfig::default()`
    /// byte-for-byte — the pre-R5 status quo, pinned through the
    /// serialized form so any drift in any field fails here.
    #[test]
    fn loop_config_none_resolves_to_default_config_exactly() {
        let resolved = ChildLoopConfig::resolve(None);
        assert_eq!(
            serde_json::to_value(&resolved).expect("serializes"),
            serde_json::to_value(AgentLoopConfig::default()).expect("serializes"),
            "None must be byte-for-byte AgentLoopConfig::default()",
        );
        // The all-unset subset is identical to None: unset fields defer
        // to the library default, never to an invented value.
        let empty = ChildLoopConfig {
            max_iterations: None,
            step_timeout_secs: None,
            linger_secs: None,
        };
        assert_eq!(
            serde_json::to_value(ChildLoopConfig::resolve(Some(empty))).expect("serializes"),
            serde_json::to_value(AgentLoopConfig::default()).expect("serializes"),
        );
    }

    /// Set fields land on exactly their `AgentLoopConfig` counterparts
    /// (seconds → `Duration`, linger seconds → `LingerPolicy.deadline`)
    /// and nothing else moves off the default.
    #[test]
    fn loop_config_overrides_apply_onto_default() {
        let resolved = ChildLoopConfig::resolve(Some(ChildLoopConfig {
            max_iterations: Some(7),
            step_timeout_secs: Some(90),
            linger_secs: Some(45),
        }));
        assert_eq!(resolved.max_iterations, Some(7));
        assert_eq!(resolved.step_timeout, Some(Duration::from_secs(90)));
        assert_eq!(
            resolved.linger,
            Some(LingerPolicy {
                deadline: Duration::from_secs(45),
            }),
        );
        // The harness-only knobs are untouched library defaults.
        let default = AgentLoopConfig::default();
        assert_eq!(
            resolved.schema_attempt_budget,
            default.schema_attempt_budget
        );
        assert_eq!(resolved.schema_tool_name, default.schema_tool_name);
        assert_eq!(resolved.cache_key, default.cache_key);
        assert_eq!(
            resolved.auto_compact_keep_recent_turns,
            default.auto_compact_keep_recent_turns,
        );
        assert_eq!(resolved.context_window_limit, default.context_window_limit);
        assert!(resolved.output_schema.is_none());
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
            "loop_confg": {},
        }));
        let err = result.expect_err("unknown field must be rejected");
        assert!(
            err.to_string().contains("loop_confg"),
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

    fn policy(depth: u32) -> ChildPolicy {
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: depth,
                max_concurrent_children: 8,
            },
            inbound_capacity: 16,
            loop_config: None,
        }
    }

    /// The default derivation is inherit-with-decrement: depth drops by
    /// exactly one level, every other field is inherited unchanged.
    #[test]
    fn grant_for_child_default_inherits_with_decrement() {
        let grant = policy(3).grant_for_child(None).expect("grant");
        assert_eq!(grant.delegation.remaining_depth, 2);
        assert_eq!(grant.delegation.max_concurrent_children, 8);
        assert_eq!(grant.messaging, MessagingScope::SiblingsAndParent);
        assert_eq!(grant.inbound_capacity, 16);

        // Chained derivation reaches a leaf in exactly `depth` steps.
        let leaf = grant.grant_for_child(None).expect("grandchild grant");
        assert_eq!(leaf.delegation.remaining_depth, 1);
        let leafer = leaf.grant_for_child(None).expect("great-grandchild grant");
        assert_eq!(leafer.delegation.remaining_depth, 0);
        assert_eq!(
            leafer.grant_for_child(None),
            Err(PolicyNarrowingError::DepthExhausted),
            "a zero-depth grant must refuse to mint children",
        );
    }

    /// A leaf spawner is refused even when the request itself is tiny —
    /// depth exhaustion is checked before any narrowing.
    #[test]
    fn grant_for_child_depth_exhausted_refuses_requests_too() {
        let err = policy(0)
            .grant_for_child(Some(policy(0)))
            .expect_err("leaf may not grant");
        assert_eq!(err, PolicyNarrowingError::DepthExhausted);
        assert!(
            err.to_string().contains("remaining_depth = 0"),
            "the refusal names the budget: {err}",
        );
    }

    /// Explicit narrowing is accepted when every field is within the
    /// spawner's own grant, and the request is used verbatim.
    #[test]
    fn grant_for_child_accepts_valid_narrowing() {
        let requested = ChildPolicy {
            messaging: MessagingScope::ParentOnly,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 2,
            },
            inbound_capacity: 4,
            loop_config: None,
        };
        let grant = policy(3)
            .grant_for_child(Some(requested.clone()))
            .expect("valid narrowing accepted");
        assert_eq!(grant, requested);
    }

    /// R5 derivation: the default inherit-with-decrement carries the
    /// spawner's `loop_config` through unchanged, level after level.
    #[test]
    fn grant_for_child_inherits_loop_config_unchanged() {
        let configured = ChildLoopConfig {
            max_iterations: Some(4),
            step_timeout_secs: Some(60),
            linger_secs: Some(15),
        };
        let mut spawner = policy(3);
        spawner.loop_config = Some(configured);

        let grant = spawner.grant_for_child(None).expect("grant");
        assert_eq!(grant.loop_config, Some(configured));
        let grandchild = grant.grant_for_child(None).expect("grandchild grant");
        assert_eq!(
            grandchild.loop_config,
            Some(configured),
            "loop_config passes through every derivation level unchanged",
        );
    }

    /// R5: a per-spawn request may set any `loop_config` regardless of
    /// the spawner's own — loop config is execution shaping, not
    /// authority, so it is exempt from the narrowing checks (the
    /// delegation/messaging fields of the same request are still
    /// enforced).
    #[test]
    fn grant_for_child_request_sets_loop_config_freely() {
        // Spawner has no loop_config; the request grants one anyway —
        // including a linger, the field that lets a mid-tree child wait
        // for its own children's late results.
        let spawner = policy(2);
        assert_eq!(spawner.loop_config, None);
        let mut requested = policy(1);
        requested.loop_config = Some(ChildLoopConfig {
            max_iterations: Some(100),
            step_timeout_secs: Some(3600),
            linger_secs: Some(120),
        });
        let grant = spawner
            .grant_for_child(Some(requested.clone()))
            .expect("loop_config is not a narrowing axis");
        assert_eq!(grant, requested);

        // And the reverse: a spawner with a tight loop_config may grant
        // a child none at all (the child then runs library defaults).
        let mut tight = policy(2);
        tight.loop_config = Some(ChildLoopConfig {
            max_iterations: Some(1),
            step_timeout_secs: Some(1),
            linger_secs: None,
        });
        let unset = tight
            .grant_for_child(Some(policy(1)))
            .expect("clearing loop_config is allowed");
        assert_eq!(unset.loop_config, None);
    }

    /// Every widening direction is refused with the typed violation that
    /// names the caller's own budget.
    #[test]
    fn grant_for_child_refuses_every_widening() {
        let own = policy(2); // depth 2, mcc 8, siblings_and_parent, inbound 16

        let mut depth_widened = own.clone();
        depth_widened.delegation.remaining_depth = 2; // must be ≤ 1
        assert_eq!(
            own.grant_for_child(Some(depth_widened)),
            Err(PolicyNarrowingError::DepthExceeded {
                requested: 2,
                granted: 2,
            }),
        );
        let err = PolicyNarrowingError::DepthExceeded {
            requested: 2,
            granted: 2,
        };
        assert!(
            err.to_string().contains("at most 1"),
            "depth refusal names the strict decrement: {err}",
        );

        let mut mcc_widened = own.clone();
        mcc_widened.delegation.remaining_depth = 1;
        mcc_widened.delegation.max_concurrent_children = 9;
        assert_eq!(
            own.grant_for_child(Some(mcc_widened)),
            Err(PolicyNarrowingError::ConcurrencyExceeded {
                requested: 9,
                granted: 8,
            }),
        );

        let mut inbound_widened = own.clone();
        inbound_widened.delegation.remaining_depth = 1;
        inbound_widened.inbound_capacity = 17;
        assert_eq!(
            own.grant_for_child(Some(inbound_widened)),
            Err(PolicyNarrowingError::InboundCapacityExceeded {
                requested: 17,
                granted: 16,
            }),
        );

        let mut zero_inbound = own.clone();
        zero_inbound.delegation.remaining_depth = 1;
        zero_inbound.inbound_capacity = 0;
        assert_eq!(
            own.grant_for_child(Some(zero_inbound)),
            Err(PolicyNarrowingError::ZeroInboundCapacity),
        );

        let mut narrow_parent = own;
        narrow_parent.messaging = MessagingScope::ParentOnly;
        let mut scope_widened = narrow_parent.clone();
        scope_widened.delegation.remaining_depth = 1;
        scope_widened.messaging = MessagingScope::SiblingsAndParent;
        assert_eq!(
            narrow_parent.grant_for_child(Some(scope_widened)),
            Err(PolicyNarrowingError::ScopeWidened {
                requested: MessagingScope::SiblingsAndParent,
                granted: MessagingScope::ParentOnly,
            }),
        );
    }

    /// Scope containment is the strict nesting `None` ⊂ `ParentOnly` ⊂
    /// `SiblingsAndParent`.
    #[test]
    fn messaging_scope_containment_is_total_nesting() {
        use MessagingScope::{None as ScopeNone, ParentOnly, SiblingsAndParent};
        for scope in [ScopeNone, ParentOnly, SiblingsAndParent] {
            assert!(scope.is_within(scope), "{scope:?} is within itself");
            assert!(scope.is_within(SiblingsAndParent));
            assert!(ScopeNone.is_within(scope));
        }
        assert!(!SiblingsAndParent.is_within(ParentOnly));
        assert!(!SiblingsAndParent.is_within(ScopeNone));
        assert!(!ParentOnly.is_within(ScopeNone));
        assert!(ParentOnly.is_within(SiblingsAndParent));
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
