//! Atomic projection of local tool-call and output pairs.

use std::collections::{HashMap, HashSet};

use crate::provider::request::ToolCallKind;
use crate::provider::response_item::{KnownResponseItemKind, ResponseItem};
use crate::session::events::{EventId, SessionEvent, ToolCallEvent};

use super::{canonical_tool_call, unresolved_local_tool_calls, without_orphan_local_tool_outputs};

/// Project local call/output pairs without letting an event-level edit split
/// the pair presented to a provider.
///
/// `projected` is an ordered subset or rewrite of `source` (for example the
/// durable prompt view after compaction and suppression marks). Every matched
/// source pair is identified by exact event and within-event occurrence, so
/// duplicate call IDs cannot erase unrelated work. When only one occurrence
/// survives the projection, that half is removed. Genuine unresolved calls
/// remain available to resume repair.
///
/// Canonical response items are filtered individually, so removing one split
/// call never discards unrelated assistant text, reasoning, or other items.
pub(crate) fn atomic_local_tool_projection(
    source: &[SessionEvent],
    projected: Vec<SessionEvent>,
) -> Vec<SessionEvent> {
    let source_inventory = LocalToolInventory::from_events(source);
    let projected_inventory = LocalToolInventory::from_events(&projected);
    let mut blocked_calls = HashSet::new();
    let mut blocked_outputs = HashSet::new();
    for (call, output) in source_inventory.pairs {
        let call_present = projected_inventory.calls.contains(&call);
        let output_present = projected_inventory.outputs.contains(&output);
        if call_present != output_present {
            blocked_calls.insert(call);
            blocked_outputs.insert(output);
        }
    }

    let projected = remove_local_tool_occurrences(projected, &blocked_calls, &blocked_outputs);
    without_orphan_local_tool_outputs(projected)
}

/// Return unresolved calls from the durable effective prompt view.
///
/// Compaction and suppression are applied before call resolution, then
/// [`atomic_local_tool_projection`] removes any surviving half of a pair split
/// by those marks. This is the authority used by resume repair and identity
/// fork seeding; scanning the append-only audit rows directly would synthesize
/// a visible output for a call the prompt deliberately hides.
pub(crate) fn unresolved_effective_local_tool_calls(events: &[SessionEvent]) -> Vec<ToolCallEvent> {
    let artifacts = crate::session::persistence::ReplayArtifacts::from_events(events.to_vec());
    let visible = artifacts
        .events
        .iter()
        .filter(|event| {
            let id = &event.base().id;
            !artifacts.superseded_event_ids.contains(id)
                && !artifacts.suppressed_event_ids.contains(id)
        })
        .cloned()
        .collect();
    let projected = atomic_local_tool_projection(&artifacts.events, visible);
    unresolved_local_tool_calls(&projected)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LocalToolKey {
    call_id: String,
    kind: LocalToolKind,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum LocalToolKind {
    Function,
    Custom,
}

impl From<ToolCallKind> for LocalToolKind {
    fn from(kind: ToolCallKind) -> Self {
        match kind {
            ToolCallKind::Function => Self::Function,
            ToolCallKind::Custom => Self::Custom,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LocalCallLocation {
    event_id: EventId,
    key: LocalToolKey,
    ordinal: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum LocalOutputLocation {
    Canonical {
        event_id: EventId,
        key: LocalToolKey,
        ordinal: usize,
    },
    Legacy {
        event_id: EventId,
    },
}

#[derive(Debug)]
struct PendingLocalCall {
    call: ToolCallEvent,
    location: LocalCallLocation,
}

#[derive(Debug, Default)]
struct LocalToolInventory {
    calls: HashSet<LocalCallLocation>,
    outputs: HashSet<LocalOutputLocation>,
    pairs: Vec<(LocalCallLocation, LocalOutputLocation)>,
}

impl LocalToolInventory {
    fn from_events(events: &[SessionEvent]) -> Self {
        let mut inventory = Self::default();
        let mut pending = Vec::new();
        for event in events {
            inventory.absorb_event(&mut pending, event);
        }
        inventory
    }

    fn absorb_event(&mut self, pending: &mut Vec<PendingLocalCall>, event: &SessionEvent) {
        match event {
            SessionEvent::ToolResult {
                base, tool_call_id, ..
            } => {
                let output = LocalOutputLocation::Legacy {
                    event_id: base.id.clone(),
                };
                self.outputs.insert(output.clone());
                if let Some(call) = resolve_pending_occurrence(pending, tool_call_id, None) {
                    self.pairs.push((call.location, output));
                }
            }
            SessionEvent::AssistantMessage {
                base,
                response_items,
                tool_calls,
                ..
            } => {
                let mut call_ordinals = HashMap::new();
                let mut output_ordinals = HashMap::new();
                if response_items.is_empty() {
                    for call in tool_calls {
                        self.open_call(pending, &base.id, call.clone(), &mut call_ordinals);
                    }
                    return;
                }
                for entry in response_items {
                    if let Some(call) = canonical_tool_call(&entry.item) {
                        self.open_call(pending, &base.id, call, &mut call_ordinals);
                    } else if let Some(key) = local_response_item_key(&entry.item) {
                        self.close_canonical_output(pending, &base.id, &key, &mut output_ordinals);
                    }
                }
            }
            _ => {}
        }
    }

    fn open_call(
        &mut self,
        pending: &mut Vec<PendingLocalCall>,
        event_id: &EventId,
        call: ToolCallEvent,
        ordinals: &mut HashMap<LocalToolKey, usize>,
    ) {
        let key = LocalToolKey::from_call(&call);
        let ordinal = next_ordinal(ordinals, &key);
        let location = LocalCallLocation {
            event_id: event_id.clone(),
            key,
            ordinal,
        };
        self.calls.insert(location.clone());
        pending.push(PendingLocalCall { call, location });
    }

    fn close_canonical_output(
        &mut self,
        pending: &mut Vec<PendingLocalCall>,
        event_id: &EventId,
        key: &LocalToolKey,
        ordinals: &mut HashMap<LocalToolKey, usize>,
    ) {
        let ordinal = next_ordinal(ordinals, key);
        let output = LocalOutputLocation::Canonical {
            event_id: event_id.clone(),
            key: key.clone(),
            ordinal,
        };
        self.outputs.insert(output.clone());
        if let Some(call) = resolve_pending_occurrence(pending, &key.call_id, Some(key.kind)) {
            self.pairs.push((call.location, output));
        }
    }
}

impl LocalToolKey {
    fn from_call(call: &ToolCallEvent) -> Self {
        Self {
            call_id: call.call_id.clone(),
            kind: call.kind.into(),
        }
    }
}

fn next_ordinal(ordinals: &mut HashMap<LocalToolKey, usize>, key: &LocalToolKey) -> usize {
    let ordinal = ordinals.entry(key.clone()).or_default();
    let current = *ordinal;
    *ordinal = ordinal.saturating_add(1);
    current
}

fn resolve_pending_occurrence(
    pending: &mut Vec<PendingLocalCall>,
    call_id: &str,
    kind: Option<LocalToolKind>,
) -> Option<PendingLocalCall> {
    let index = pending.iter().position(|entry| {
        entry.call.call_id == call_id
            && kind.is_none_or(|kind| LocalToolKind::from(entry.call.kind) == kind)
    })?;
    Some(pending.remove(index))
}

fn remove_local_tool_occurrences(
    events: Vec<SessionEvent>,
    blocked_calls: &HashSet<LocalCallLocation>,
    blocked_outputs: &HashSet<LocalOutputLocation>,
) -> Vec<SessionEvent> {
    let mut retained = Vec::with_capacity(events.len());
    for mut event in events {
        match &mut event {
            SessionEvent::AssistantMessage {
                base,
                response_items,
                tool_calls,
                ..
            } if response_items.is_empty() => {
                let mut ordinals = HashMap::new();
                tool_calls.retain(|call| {
                    let key = LocalToolKey::from_call(call);
                    let ordinal = next_ordinal(&mut ordinals, &key);
                    !blocked_calls.contains(&LocalCallLocation {
                        event_id: base.id.clone(),
                        key,
                        ordinal,
                    })
                });
            }
            SessionEvent::AssistantMessage {
                base,
                response_items,
                ..
            } => {
                let mut call_ordinals = HashMap::new();
                let mut output_ordinals = HashMap::new();
                response_items.retain(|entry| {
                    if let Some(call) = canonical_tool_call(&entry.item) {
                        let key = LocalToolKey::from_call(&call);
                        let ordinal = next_ordinal(&mut call_ordinals, &key);
                        return !blocked_calls.contains(&LocalCallLocation {
                            event_id: base.id.clone(),
                            key,
                            ordinal,
                        });
                    }
                    let Some(key) = local_response_item_key(&entry.item) else {
                        return true;
                    };
                    let ordinal = next_ordinal(&mut output_ordinals, &key);
                    !blocked_outputs.contains(&LocalOutputLocation::Canonical {
                        event_id: base.id.clone(),
                        key,
                        ordinal,
                    })
                });
                if response_items.is_empty() {
                    continue;
                }
            }
            SessionEvent::ToolResult { base, .. }
                if blocked_outputs.contains(&LocalOutputLocation::Legacy {
                    event_id: base.id.clone(),
                }) =>
            {
                continue;
            }
            _ => {}
        }
        retained.push(event);
    }
    retained
}

fn local_response_item_key(item: &ResponseItem) -> Option<LocalToolKey> {
    let (call_id, kind) = match item {
        ResponseItem::FunctionCall(call) => (call.call_id(), LocalToolKind::Function),
        ResponseItem::CustomToolCall(call) => (call.call_id(), LocalToolKind::Custom),
        ResponseItem::Known(known) if known.kind() == KnownResponseItemKind::FunctionCallOutput => {
            (
                item.raw().get("call_id")?.as_str()?,
                LocalToolKind::Function,
            )
        }
        ResponseItem::Known(known)
            if known.kind() == KnownResponseItemKind::CustomToolCallOutput =>
        {
            (item.raw().get("call_id")?.as_str()?, LocalToolKind::Custom)
        }
        _ => return None,
    };
    Some(LocalToolKey {
        call_id: call_id.to_owned(),
        kind,
    })
}
