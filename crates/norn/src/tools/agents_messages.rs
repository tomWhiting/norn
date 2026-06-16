//! Message-edge derivation for the `agents` status tool.
//!
//! The `agents` tool's `messages` action renders inter-agent messaging
//! activity as **edges** (sender → recipient summaries) derived
//! read-only from the `agent_message.sent`, `agent_message.delivered`,
//! `agent_message.queued`, and `agent_message.dequeued`
//! [`SessionEvent::Custom`] audit events already persisted in the calling
//! agent's own event store. Nothing here emits events or
//! touches the router — this is a pure projection of the audit trail.
//!
//! # What one store can honestly attest
//!
//! The Wave 3 audit contract is dual-store: a `sent` record lands in the
//! sender's store **and** the scope-granting parent's store; a
//! `delivered` record lands in the recipient's store only. The caller's
//! own store therefore contains:
//!
//! - `sent`/`queued` records for messages the caller sent, and for messages its
//!   **direct children** exchanged (the caller granted their scope);
//! - `delivered`/`dequeued` records for messages delivered **to the caller**.
//!
//! Edges render exactly what the store attests and mark everything else
//! unknown: `sent`/`delivered` counts are JSON `null` when the caller's
//! store cannot hold the corresponding audit record (e.g. delivery to a
//! child is recorded in the child's store, not the caller's). `null`
//! means "not knowable from your audit store", never zero.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::agent::registry::AgentRegistry;
use crate::agent::{
    AGENT_MESSAGE_DEQUEUED_EVENT_TYPE, AGENT_MESSAGE_QUEUED_EVENT_TYPE,
    PendingAgentMessageLifecycle,
};
use crate::provider::agent_event::{
    AGENT_MESSAGE_DELIVERED_EVENT_TYPE, AGENT_MESSAGE_SENT_EVENT_TYPE, AgentMessageLifecycle,
};
use crate::session::events::SessionEvent;

/// Result of deriving message edges from one store's audit events.
pub(crate) struct MessageEdges {
    /// Rendered per-edge summaries, ordered by first activity (ties by
    /// sender then recipient id) so output is deterministic.
    pub(crate) edges: Vec<Value>,
    /// Number of `agent_message.*` Custom events whose payload did not
    /// parse as the phase their `event_type` declares. Surfaced in the
    /// tool output (and warned per event) — never silently skipped.
    pub(crate) malformed: usize,
}

/// Accumulator for one `(from_id, to_id)` edge.
struct EdgeAccum {
    /// Labels recorded on the most recent `sent` event (registry ground
    /// truth at send time). `None` until a `sent` record is seen.
    from_label: Option<String>,
    to_label: Option<String>,
    /// Message ids seen per phase — counts are deduplicated by id so a
    /// replayed/duplicated audit line can never inflate an edge.
    sent_ids: HashSet<Uuid>,
    delivered_ids: HashSet<Uuid>,
    queued_ids: HashSet<Uuid>,
    dequeued_ids: HashSet<Uuid>,
    /// Per-kind send counts (deduplicated alongside `sent_ids`).
    steer: u64,
    update: u64,
    /// Earliest and latest activity across both phases.
    first_at: DateTime<Utc>,
    last_at: DateTime<Utc>,
    /// Highest router-minted sequence number seen on the edge.
    last_seq: u64,
}

impl EdgeAccum {
    fn new(at: DateTime<Utc>) -> Self {
        Self {
            from_label: None,
            to_label: None,
            sent_ids: HashSet::new(),
            delivered_ids: HashSet::new(),
            queued_ids: HashSet::new(),
            dequeued_ids: HashSet::new(),
            steer: 0,
            update: 0,
            first_at: at,
            last_at: at,
            last_seq: 0,
        }
    }

    fn observe(&mut self, at: DateTime<Utc>, seq: u64) {
        if at < self.first_at {
            self.first_at = at;
        }
        if at > self.last_at {
            self.last_at = at;
        }
        if seq > self.last_seq {
            self.last_seq = seq;
        }
    }
}

/// Resolve a display label for `id` when no send-time label is on
/// record: live/terminal registry path, else tombstone path, else the
/// bare UUID. Mirrors the attribution fallback the coordination tools
/// use, so every surface tells the same story about the same agent.
fn fallback_label(registry: &AgentRegistry, id: Uuid) -> String {
    if let Some(entry) = registry.get(id) {
        return entry.path;
    }
    if let Some(tombstone) = registry.tombstone(id) {
        return tombstone.path;
    }
    id.to_string()
}

/// Derive per-edge message summaries from `events` (the caller's own
/// store), resolving labels for delivered-only edges against `registry`.
pub(crate) fn derive_message_edges(
    events: &[SessionEvent],
    caller: Uuid,
    registry: &AgentRegistry,
) -> MessageEdges {
    let mut accums: HashMap<(Uuid, Uuid), EdgeAccum> = HashMap::new();
    let mut message_edges: HashMap<Uuid, (Uuid, Uuid)> = HashMap::new();
    let mut malformed = 0usize;

    for event in events {
        let SessionEvent::Custom {
            event_type, data, ..
        } = event
        else {
            continue;
        };
        let expects_sent = match event_type.as_str() {
            AGENT_MESSAGE_SENT_EVENT_TYPE => Some(true),
            AGENT_MESSAGE_DELIVERED_EVENT_TYPE => Some(false),
            _ => None,
        };
        if expects_sent.is_none() {
            match event_type.as_str() {
                AGENT_MESSAGE_QUEUED_EVENT_TYPE | AGENT_MESSAGE_DEQUEUED_EVENT_TYPE => {
                    match serde_json::from_value::<PendingAgentMessageLifecycle>(data.clone()) {
                        Ok(lifecycle) => {
                            handle_pending_lifecycle(
                                event_type,
                                lifecycle,
                                &mut accums,
                                &mut message_edges,
                                &mut malformed,
                            );
                        }
                        Err(error) => {
                            malformed += 1;
                            tracing::warn!(
                                event_type = %event_type,
                                %error,
                                "agents messages: unparseable pending agent_message audit payload",
                            );
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let expects_sent = expects_sent.unwrap_or(false);
        let lifecycle: AgentMessageLifecycle = match serde_json::from_value(data.clone()) {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                malformed += 1;
                tracing::warn!(
                    event_type = %event_type,
                    %error,
                    "agents messages: unparseable agent_message audit payload",
                );
                continue;
            }
        };
        match lifecycle {
            AgentMessageLifecycle::Sent {
                message_id,
                from_id,
                from,
                to_id,
                to,
                kind,
                seq,
                sent_at,
                content: _,
            } => {
                if !expects_sent {
                    malformed += 1;
                    tracing::warn!(
                        event_type = %event_type,
                        %message_id,
                        "agents messages: event_type/phase mismatch (sent payload \
                         under a delivered event type)",
                    );
                    continue;
                }
                let accum = accums
                    .entry((from_id, to_id))
                    .or_insert_with(|| EdgeAccum::new(sent_at));
                accum.observe(sent_at, seq);
                message_edges.insert(message_id, (from_id, to_id));
                accum.from_label = Some(from);
                accum.to_label = Some(to);
                if accum.sent_ids.insert(message_id) {
                    match kind {
                        crate::r#loop::inbound::MessageKind::Steer => accum.steer += 1,
                        crate::r#loop::inbound::MessageKind::Update => accum.update += 1,
                    }
                }
            }
            AgentMessageLifecycle::Delivered {
                message_id,
                from_id,
                to_id,
                seq,
                delivered_at,
            } => {
                if expects_sent {
                    malformed += 1;
                    tracing::warn!(
                        event_type = %event_type,
                        %message_id,
                        "agents messages: event_type/phase mismatch (delivered \
                         payload under a sent event type)",
                    );
                    continue;
                }
                let accum = accums
                    .entry((from_id, to_id))
                    .or_insert_with(|| EdgeAccum::new(delivered_at));
                accum.observe(delivered_at, seq);
                accum.delivered_ids.insert(message_id);
            }
        }
    }

    let mut keyed: Vec<((Uuid, Uuid), EdgeAccum)> = accums.into_iter().collect();
    keyed.sort_by(|a, b| a.1.first_at.cmp(&b.1.first_at).then_with(|| a.0.cmp(&b.0)));

    let edges = keyed
        .into_iter()
        .map(|((from_id, to_id), accum)| edge_json(from_id, to_id, &accum, caller, registry))
        .collect();

    MessageEdges { edges, malformed }
}

fn handle_pending_lifecycle(
    event_type: &str,
    lifecycle: PendingAgentMessageLifecycle,
    accums: &mut HashMap<(Uuid, Uuid), EdgeAccum>,
    message_edges: &mut HashMap<Uuid, (Uuid, Uuid)>,
    malformed: &mut usize,
) {
    match lifecycle {
        PendingAgentMessageLifecycle::Queued {
            message_id,
            from_id,
            from,
            to_id,
            to,
            kind,
            queued_at,
            ..
        } => {
            if event_type != AGENT_MESSAGE_QUEUED_EVENT_TYPE {
                *malformed += 1;
                tracing::warn!(
                    event_type,
                    %message_id,
                    "agents messages: event_type/phase mismatch (queued payload \
                     under a non-queued event type)",
                );
                return;
            }
            message_edges.insert(message_id, (from_id, to_id));
            let accum = accums
                .entry((from_id, to_id))
                .or_insert_with(|| EdgeAccum::new(queued_at));
            accum.observe(queued_at, 0);
            accum.from_label = Some(from);
            accum.to_label = Some(to);
            if accum.queued_ids.insert(message_id) {
                match kind {
                    crate::r#loop::inbound::MessageKind::Steer => accum.steer += 1,
                    crate::r#loop::inbound::MessageKind::Update => accum.update += 1,
                }
            }
        }
        PendingAgentMessageLifecycle::Dequeued {
            message_id,
            to_id,
            dequeued_at,
        } => {
            if event_type != AGENT_MESSAGE_DEQUEUED_EVENT_TYPE {
                *malformed += 1;
                tracing::warn!(
                    event_type,
                    %message_id,
                    "agents messages: event_type/phase mismatch (dequeued payload \
                     under a non-dequeued event type)",
                );
                return;
            }
            let Some((from_id, edge_to_id)) = message_edges.get(&message_id).copied() else {
                *malformed += 1;
                tracing::warn!(
                    event_type,
                    %message_id,
                    "agents messages: dequeued audit has no prior queued/sent edge in this store",
                );
                return;
            };
            if edge_to_id != to_id {
                *malformed += 1;
                tracing::warn!(
                    event_type,
                    %message_id,
                    expected_to = %edge_to_id,
                    actual_to = %to_id,
                    "agents messages: dequeued recipient does not match queued edge",
                );
                return;
            }
            let accum = accums
                .entry((from_id, to_id))
                .or_insert_with(|| EdgeAccum::new(dequeued_at));
            accum.observe(dequeued_at, 0);
            accum.dequeued_ids.insert(message_id);
        }
    }
}

/// Render one edge. `sent` is `null` when this store holds no send
/// audit for the edge (the caller is neither the sender nor the
/// sender's scope-granting parent); `delivered` is `null` when delivery
/// is not knowable from this store (the caller is not the recipient and
/// no delivered audit is present). `kinds` accompanies a known `sent`
/// count only — the delivered phase does not carry a kind.
fn edge_json(
    from_id: Uuid,
    to_id: Uuid,
    accum: &EdgeAccum,
    caller: Uuid,
    registry: &AgentRegistry,
) -> Value {
    let from = accum
        .from_label
        .clone()
        .unwrap_or_else(|| fallback_label(registry, from_id));
    let to = accum
        .to_label
        .clone()
        .unwrap_or_else(|| fallback_label(registry, to_id));
    let mut edge = serde_json::json!({
        "from": from,
        "from_id": from_id.to_string(),
        "to": to,
        "to_id": to_id.to_string(),
        "sent": Value::Null,
        "delivered": Value::Null,
        "queued": Value::Null,
        "dequeued": Value::Null,
        "first_at": accum.first_at.to_rfc3339(),
        "last_at": accum.last_at.to_rfc3339(),
        "last_seq": accum.last_seq,
    });
    if !accum.sent_ids.is_empty() {
        edge["sent"] = serde_json::json!(accum.sent_ids.len());
        edge["kinds"] = serde_json::json!({
            "steer": accum.steer,
            "update": accum.update,
        });
    }
    if to_id == caller || !accum.delivered_ids.is_empty() {
        edge["delivered"] = serde_json::json!(accum.delivered_ids.len());
    }
    if !accum.queued_ids.is_empty() {
        edge["queued"] = serde_json::json!(accum.queued_ids.len());
    }
    if to_id == caller || !accum.dequeued_ids.is_empty() {
        edge["dequeued"] = serde_json::json!(accum.dequeued_ids.len());
    }
    edge
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_arguments
)]
mod tests {
    use super::*;
    use crate::r#loop::inbound::MessageKind;
    use crate::session::events::EventBase;

    fn sent_event(
        message_id: Uuid,
        from_id: Uuid,
        from: &str,
        to_id: Uuid,
        to: &str,
        kind: MessageKind,
        seq: u64,
        sent_at: &str,
    ) -> SessionEvent {
        let lifecycle = AgentMessageLifecycle::Sent {
            message_id,
            from_id,
            from: from.to_owned(),
            to_id,
            to: to.to_owned(),
            kind,
            seq,
            content: "payload".to_owned(),
            sent_at: DateTime::parse_from_rfc3339(sent_at).unwrap().to_utc(),
        };
        SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: AGENT_MESSAGE_SENT_EVENT_TYPE.to_owned(),
            data: serde_json::to_value(&lifecycle).unwrap(),
        }
    }

    fn delivered_event(
        message_id: Uuid,
        from_id: Uuid,
        to_id: Uuid,
        seq: u64,
        delivered_at: &str,
    ) -> SessionEvent {
        let lifecycle = AgentMessageLifecycle::Delivered {
            message_id,
            from_id,
            to_id,
            seq,
            delivered_at: DateTime::parse_from_rfc3339(delivered_at).unwrap().to_utc(),
        };
        SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: AGENT_MESSAGE_DELIVERED_EVENT_TYPE.to_owned(),
            data: serde_json::to_value(&lifecycle).unwrap(),
        }
    }

    fn queued_event(
        message_id: Uuid,
        from_id: Uuid,
        from: &str,
        to_id: Uuid,
        to: &str,
        kind: MessageKind,
        queued_at: &str,
    ) -> SessionEvent {
        let lifecycle = PendingAgentMessageLifecycle::Queued {
            message_id,
            from_id,
            from: from.to_owned(),
            role: None,
            to_id,
            to: to.to_owned(),
            kind,
            content: "payload".to_owned(),
            queued_at: DateTime::parse_from_rfc3339(queued_at).unwrap().to_utc(),
        };
        SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: AGENT_MESSAGE_QUEUED_EVENT_TYPE.to_owned(),
            data: serde_json::to_value(&lifecycle).unwrap(),
        }
    }

    fn dequeued_event(message_id: Uuid, to_id: Uuid, dequeued_at: &str) -> SessionEvent {
        let lifecycle = PendingAgentMessageLifecycle::Dequeued {
            message_id,
            to_id,
            dequeued_at: DateTime::parse_from_rfc3339(dequeued_at).unwrap().to_utc(),
        };
        SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: AGENT_MESSAGE_DEQUEUED_EVENT_TYPE.to_owned(),
            data: serde_json::to_value(&lifecycle).unwrap(),
        }
    }

    fn empty_registry() -> AgentRegistry {
        AgentRegistry::new()
    }

    /// `fallback_label` prefers the live registry path, then the
    /// tombstone path, and only then the bare UUID — pins the two
    /// registry-backed branches the store-driven tests never reach.
    #[test]
    fn fallback_label_resolves_live_and_tombstone_paths() {
        use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
        let shared = AgentRegistry::shared();
        let policy = ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 2,
            },
            inbound_capacity: 1,
            loop_config: None,
        };

        let live = AgentRegistry::reserve(
            &shared,
            "/root/live".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
            policy.clone(),
            None,
        )
        .expect("reserve live");
        let live_id = live.id();
        live.confirm().expect("confirm live");

        let gone = AgentRegistry::reserve(
            &shared,
            "/root/gone".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
            policy,
            None,
        )
        .expect("reserve gone");
        let gone_id = gone.id();
        gone.confirm().expect("confirm gone");
        shared.write().mark_failed(gone_id).expect("mark failed");
        assert!(shared.write().remove_terminal(gone_id), "reclaim");

        let registry = shared.read();
        assert_eq!(fallback_label(&registry, live_id), "/root/live");
        assert_eq!(
            fallback_label(&registry, gone_id),
            "/root/gone",
            "a reclaimed agent resolves through its tombstone",
        );
        let unknown = Uuid::new_v4();
        assert_eq!(fallback_label(&registry, unknown), unknown.to_string());
    }

    #[test]
    fn empty_store_yields_no_edges() {
        let out = derive_message_edges(&[], Uuid::new_v4(), &empty_registry());
        assert!(out.edges.is_empty());
        assert_eq!(out.malformed, 0);
    }

    #[test]
    fn non_message_custom_events_are_ignored() {
        let events = vec![SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: "subagent.started".to_owned(),
            data: serde_json::json!({ "anything": true }),
        }];
        let out = derive_message_edges(&events, Uuid::new_v4(), &empty_registry());
        assert!(out.edges.is_empty());
        assert_eq!(out.malformed, 0);
    }

    /// A child→caller edge: the caller's store carries the granted
    /// `sent` copy and the recipient-side `delivered` copy — both counts
    /// known, kinds tallied per deduplicated message.
    #[test]
    fn child_to_caller_edge_counts_sent_and_delivered() {
        let caller = Uuid::from_u128(1);
        let child = Uuid::from_u128(2);
        let m1 = Uuid::from_u128(10);
        let m2 = Uuid::from_u128(11);
        let events = vec![
            sent_event(
                m1,
                child,
                "/root/spawn/c",
                caller,
                "root",
                MessageKind::Steer,
                1,
                "2026-06-12T10:00:00Z",
            ),
            sent_event(
                m2,
                child,
                "/root/spawn/c",
                caller,
                "root",
                MessageKind::Update,
                2,
                "2026-06-12T10:00:05Z",
            ),
            delivered_event(m1, child, caller, 1, "2026-06-12T10:00:01Z"),
        ];
        let out = derive_message_edges(&events, caller, &empty_registry());
        assert_eq!(out.malformed, 0);
        assert_eq!(out.edges.len(), 1);
        let edge = &out.edges[0];
        assert_eq!(edge["from"], "/root/spawn/c");
        assert_eq!(edge["from_id"], child.to_string());
        assert_eq!(edge["to"], "root");
        assert_eq!(edge["to_id"], caller.to_string());
        assert_eq!(edge["sent"], 2);
        assert_eq!(edge["kinds"]["steer"], 1);
        assert_eq!(edge["kinds"]["update"], 1);
        assert_eq!(edge["delivered"], 1);
        assert_eq!(edge["last_seq"], 2);
        assert_eq!(edge["first_at"], "2026-06-12T10:00:00+00:00");
        assert_eq!(edge["last_at"], "2026-06-12T10:00:05+00:00");
    }

    /// Sibling↔sibling traffic appears in the granting parent's store
    /// as `sent` audit only; delivery happens in the recipient child's
    /// store, so `delivered` must be `null` (unknown), never zero.
    #[test]
    fn child_to_child_edge_has_unknown_delivery() {
        let caller = Uuid::from_u128(1);
        let a = Uuid::from_u128(2);
        let b = Uuid::from_u128(3);
        let events = vec![sent_event(
            Uuid::from_u128(10),
            a,
            "/root/spawn/a",
            b,
            "/root/spawn/b",
            MessageKind::Steer,
            1,
            "2026-06-12T10:00:00Z",
        )];
        let out = derive_message_edges(&events, caller, &empty_registry());
        assert_eq!(out.edges.len(), 1);
        let edge = &out.edges[0];
        assert_eq!(edge["sent"], 1);
        assert!(
            edge["delivered"].is_null(),
            "delivery to a non-caller recipient is unknowable from this store: {edge}",
        );
    }

    #[test]
    fn queued_and_dequeued_edge_counts_are_visible() {
        let caller = Uuid::from_u128(1);
        let child = Uuid::from_u128(2);
        let message = Uuid::from_u128(10);
        let events = vec![
            queued_event(
                message,
                child,
                "/root/spawn/c",
                caller,
                "root",
                MessageKind::Update,
                "2026-06-12T10:00:00Z",
            ),
            dequeued_event(message, caller, "2026-06-12T10:00:03Z"),
        ];

        let out = derive_message_edges(&events, caller, &empty_registry());

        assert_eq!(out.malformed, 0);
        assert_eq!(out.edges.len(), 1);
        let edge = &out.edges[0];
        assert!(edge["sent"].is_null(), "queued is distinct from sent");
        assert_eq!(edge["queued"], 1);
        assert_eq!(edge["dequeued"], 1);
        assert_eq!(edge["first_at"], "2026-06-12T10:00:00+00:00");
        assert_eq!(edge["last_at"], "2026-06-12T10:00:03+00:00");
    }

    /// A caller that received a message from its parent holds only the
    /// `delivered` record: `sent` is `null` (the send audit lives in the
    /// parent's and grandparent's stores), `delivered` is counted, and
    /// labels fall back to registry/UUID resolution.
    #[test]
    fn delivered_only_edge_reports_unknown_sent_and_fallback_labels() {
        let caller = Uuid::from_u128(1);
        let parent = Uuid::from_u128(9);
        let events = vec![delivered_event(
            Uuid::from_u128(10),
            parent,
            caller,
            3,
            "2026-06-12T10:00:00Z",
        )];
        let out = derive_message_edges(&events, caller, &empty_registry());
        assert_eq!(out.edges.len(), 1);
        let edge = &out.edges[0];
        assert!(edge["sent"].is_null(), "send audit absent: {edge}");
        assert!(edge.get("kinds").is_none(), "no kinds without sent: {edge}");
        assert_eq!(edge["delivered"], 1);
        // No registry record → bare UUID labels, never invented paths.
        assert_eq!(edge["from"], parent.to_string());
        assert_eq!(edge["to"], caller.to_string());
        assert_eq!(edge["last_seq"], 3);
    }

    /// Duplicate audit lines for the same message id count once.
    #[test]
    fn counts_deduplicate_by_message_id() {
        let caller = Uuid::from_u128(1);
        let child = Uuid::from_u128(2);
        let m = Uuid::from_u128(10);
        let sent = sent_event(
            m,
            child,
            "/c",
            caller,
            "root",
            MessageKind::Steer,
            1,
            "2026-06-12T10:00:00Z",
        );
        let delivered = delivered_event(m, child, caller, 1, "2026-06-12T10:00:01Z");
        let events = vec![sent.clone(), sent, delivered.clone(), delivered];
        let out = derive_message_edges(&events, caller, &empty_registry());
        let edge = &out.edges[0];
        assert_eq!(edge["sent"], 1);
        assert_eq!(edge["kinds"]["steer"], 1);
        assert_eq!(edge["delivered"], 1);
    }

    /// Unparseable payloads and phase/event-type mismatches are counted
    /// as malformed and reported — the rest of the store still answers.
    #[test]
    fn malformed_payloads_are_counted_not_dropped_silently() {
        let caller = Uuid::from_u128(1);
        let child = Uuid::from_u128(2);
        let garbage = SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: AGENT_MESSAGE_SENT_EVENT_TYPE.to_owned(),
            data: serde_json::json!({ "phase": "nonsense" }),
        };
        // Delivered payload filed under the sent event type.
        let mismatched = {
            let lifecycle = AgentMessageLifecycle::Delivered {
                message_id: Uuid::from_u128(99),
                from_id: child,
                to_id: caller,
                seq: 1,
                delivered_at: DateTime::parse_from_rfc3339("2026-06-12T10:00:00Z")
                    .unwrap()
                    .to_utc(),
            };
            SessionEvent::Custom {
                base: EventBase::new(None),
                event_type: AGENT_MESSAGE_SENT_EVENT_TYPE.to_owned(),
                data: serde_json::to_value(&lifecycle).unwrap(),
            }
        };
        let good = sent_event(
            Uuid::from_u128(10),
            child,
            "/c",
            caller,
            "root",
            MessageKind::Update,
            1,
            "2026-06-12T10:00:00Z",
        );
        let out = derive_message_edges(&[garbage, mismatched, good], caller, &empty_registry());
        assert_eq!(out.malformed, 2);
        assert_eq!(out.edges.len(), 1);
        assert_eq!(out.edges[0]["sent"], 1);
    }

    /// Edges order deterministically by first activity, ids breaking
    /// timestamp ties.
    #[test]
    fn edges_order_by_first_activity() {
        let caller = Uuid::from_u128(1);
        let a = Uuid::from_u128(2);
        let b = Uuid::from_u128(3);
        let events = vec![
            sent_event(
                Uuid::from_u128(11),
                b,
                "/b",
                caller,
                "root",
                MessageKind::Steer,
                1,
                "2026-06-12T10:00:05Z",
            ),
            sent_event(
                Uuid::from_u128(10),
                a,
                "/a",
                caller,
                "root",
                MessageKind::Steer,
                1,
                "2026-06-12T10:00:00Z",
            ),
        ];
        let out = derive_message_edges(&events, caller, &empty_registry());
        assert_eq!(out.edges.len(), 2);
        assert_eq!(out.edges[0]["from"], "/a");
        assert_eq!(out.edges[1]["from"], "/b");
    }
}
