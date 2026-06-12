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

use serde::{Deserialize, Serialize};

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

/// Who a child agent may message through `send_message` (DECISION M1).
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
    /// `send_message` is not available (tool absent from the child's
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
    ///   contract. Messaging scope, concurrency cap, and inbound
    ///   capacity are inherited unchanged.
    /// - **`Some` (narrowing only):** the request is validated against
    ///   the spawner's own grant — depth strictly decremented
    ///   (`requested.remaining_depth ≤ self.remaining_depth - 1`),
    ///   concurrency / inbound capacity at most the spawner's own, scope
    ///   contained in the spawner's own. A parent can tighten, never
    ///   widen.
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

    fn policy(depth: u32) -> ChildPolicy {
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: depth,
                max_concurrent_children: 8,
            },
            inbound_capacity: 16,
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
        };
        let grant = policy(3)
            .grant_for_child(Some(requested.clone()))
            .expect("valid narrowing accepted");
        assert_eq!(grant, requested);
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
