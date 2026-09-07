//! Producer-bound notification summaries and live/audit identity reconciliation.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use uuid::Uuid;

use super::body::{BodyRepresentation, DisplayText};
use super::contract::{HistoryPosition, HistoryRecord, ItemId, ViewItemKind};
use super::error::ViewError;
use super::projection::SessionProjection;
use crate::r#loop::inbound::{ChannelMessage, MessageKind, frame_message};
use crate::provider::agent_event::{
    AGENT_MESSAGE_DELIVERED_EVENT_TYPE, AGENT_MESSAGE_SENT_EVENT_TYPE, AgentMessageLifecycle,
};
use crate::session::events::{EventId, SessionEvent};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct AuditKey {
    delivered: bool,
    message_id: Uuid,
    recipient: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessageAudit {
    key: AuditKey,
    sender: Uuid,
    from: String,
    seq: Option<u64>,
    user_event_id: Option<EventId>,
}

impl MessageAudit {
    fn from_lifecycle(lifecycle: &AgentMessageLifecycle) -> Self {
        match lifecycle {
            AgentMessageLifecycle::Sent {
                message_id,
                from_id,
                from,
                to_id,
                seq,
                ..
            } => Self {
                key: AuditKey {
                    delivered: false,
                    message_id: *message_id,
                    recipient: *to_id,
                },
                sender: *from_id,
                from: from.clone(),
                seq: Some(*seq),
                user_event_id: None,
            },
            AgentMessageLifecycle::Delivered {
                message_id,
                from_id,
                from,
                to_id,
                seq,
                ..
            } => Self {
                key: AuditKey {
                    delivered: true,
                    message_id: *message_id,
                    recipient: *to_id,
                },
                sender: *from_id,
                from: from.clone(),
                seq: *seq,
                user_event_id: None,
            },
        }
    }

    fn owner(&self) -> Uuid {
        if self.key.delivered {
            self.key.recipient
        } else {
            self.sender
        }
    }

    fn label(&self) -> String {
        let phase = if self.key.delivered {
            "delivered"
        } else {
            "sent"
        };
        format!("Message {phase} from {}", self.from)
    }

    fn same_attribution(&self, other: &Self) -> bool {
        self.key == other.key
            && self.sender == other.sender
            && self.from == other.from
            && self.seq == other.seq
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FramedInput {
    sender: Uuid,
    from: String,
    seq: Option<u64>,
    summary: String,
}

/// Compact facts only; the selected record's original body stays in its store.
#[derive(Clone, Debug)]
pub(crate) enum NotificationRecord {
    Input(FramedInput),
    Audit(MessageAudit),
}

impl NotificationRecord {
    pub(super) fn from_event(event: &SessionEvent) -> Option<Self> {
        match event {
            SessionEvent::UserMessage { content, .. } => framed_input(content).map(Self::Input),
            SessionEvent::Custom {
                event_type, data, ..
            } if matches!(
                event_type.as_str(),
                AGENT_MESSAGE_SENT_EVENT_TYPE | AGENT_MESSAGE_DELIVERED_EVENT_TYPE
            ) =>
            {
                let lifecycle: AgentMessageLifecycle =
                    AgentMessageLifecycle::deserialize(data).ok()?;
                if lifecycle.session_event_type() != event_type {
                    return None;
                }
                let mut audit = MessageAudit::from_lifecycle(&lifecycle);
                if audit.key.delivered {
                    audit.user_event_id = data
                        .get("user_event_id")
                        .and_then(|value| serde_json::from_value(value.clone()).ok());
                }
                Some(Self::Audit(audit))
            }
            _ => None,
        }
    }
}

struct Binding {
    audit: MessageAudit,
    ordinal: usize,
    conflicted: bool,
}

#[derive(Default)]
pub(super) struct NotificationState {
    inputs: HashMap<EventId, FramedInput>,
    bindings: HashMap<EventId, Binding>,
    audits: HashMap<AuditKey, (MessageAudit, ItemId)>,
    bound_items: HashSet<ItemId>,
    conflicted_audits: HashSet<AuditKey>,
}

impl SessionProjection {
    /// Whether this exact retained row has a validated producer delivery binding.
    /// XML-looking text, ordinary MCP inputs and legacy unbound records are false.
    #[must_use]
    pub fn is_bound_notification(&self, item: &ItemId) -> bool {
        self.notifications.bound_items.contains(item)
    }

    pub(super) fn observe_message_audit(
        &mut self,
        message: &AgentMessageLifecycle,
    ) -> Result<(), ViewError> {
        let audit = MessageAudit::from_lifecycle(message);
        if audit.owner() != self.source.agent_id {
            return Err(ViewError::AgentMismatch {
                expected: self.source.agent_id,
                actual: audit.owner(),
            });
        }
        if self
            .notifications
            .audits
            .get(&audit.key)
            .is_some_and(|(existing, ..)| existing.same_attribution(&audit))
        {
            return Ok(());
        }
        let label = audit.label();
        let text =
            serde_json::to_string(message).map_err(|source| ViewError::LiveBodyMalformed {
                referent: label.clone(),
                source,
            })?;
        let id = self.record_local_body(
            ViewItemKind::Metadata,
            &label,
            &text,
            BodyRepresentation::Json,
        )?;
        self.notifications
            .audits
            .entry(audit.key.clone())
            .or_insert((audit, id));
        Ok(())
    }

    pub(super) fn apply_notification_record(
        &mut self,
        record: &HistoryRecord,
    ) -> Result<(), ViewError> {
        let Some(facts) = record.notification.as_deref() else {
            return Ok(());
        };
        let HistoryPosition::Event { ordinal, event_id } = record.cursor.position() else {
            return Err(ViewError::AttemptMismatch);
        };
        match facts {
            NotificationRecord::Input(input) => {
                self.notifications
                    .inputs
                    .insert(event_id.clone(), input.clone());
                self.reconcile_notification(event_id);
            }
            NotificationRecord::Audit(audit) if audit.owner() == self.source.agent_id => {
                let Some(row) = record.items.first() else {
                    return Err(ViewError::AttemptMismatch);
                };
                self.reconcile_audit(audit, &row.id)?;
                if let Some(user_event_id) = &audit.user_event_id {
                    let binding = self
                        .notifications
                        .bindings
                        .entry(user_event_id.clone())
                        .or_insert_with(|| Binding {
                            audit: audit.clone(),
                            ordinal: *ordinal,
                            conflicted: self.notifications.conflicted_audits.contains(&audit.key),
                        });
                    if binding.audit != *audit
                        || self.notifications.conflicted_audits.contains(&audit.key)
                    {
                        binding.conflicted = true;
                    }
                    self.reconcile_notification(user_event_id);
                }
            }
            NotificationRecord::Audit(..) => {}
        }
        Ok(())
    }

    fn reconcile_audit(&mut self, audit: &MessageAudit, id: &ItemId) -> Result<(), ViewError> {
        let Some(row) = self.items.get_mut(id) else {
            return Err(ViewError::AttemptMismatch);
        };
        row.kind = ViewItemKind::Metadata;
        row.label = DisplayText::new(&audit.label());
        if let Some((existing, previous)) = self.notifications.audits.get(&audit.key).cloned() {
            let conflicting_binding = matches!((&existing.user_event_id, &audit.user_event_id), (Some(first), Some(second)) if first != second);
            if !existing.same_attribution(audit) || conflicting_binding {
                self.notifications
                    .conflicted_audits
                    .insert(audit.key.clone());
                if let Some(user_event_id) = &existing.user_event_id {
                    if let Some(binding) = self.notifications.bindings.get_mut(user_event_id) {
                        binding.conflicted = true;
                    }
                    self.reconcile_notification(user_event_id);
                }
            }
            if existing.same_attribution(audit) && !conflicting_binding {
                if let ItemId::Local { ordinal, .. } = &previous {
                    self.items.remove(&previous);
                    self.local_bodies.remove(ordinal);
                    self.link_alias(previous, id.clone())?;
                    self.notifications
                        .audits
                        .insert(audit.key.clone(), (audit.clone(), id.clone()));
                }
                // Distinct committed audits remain inspectable even when their
                // lifecycle identity repeats. Only the live observation retires.
                return Ok(());
            }
        } else {
            self.notifications
                .audits
                .insert(audit.key.clone(), (audit.clone(), id.clone()));
        }
        Ok(())
    }

    fn reconcile_notification(&mut self, event_id: &EventId) {
        let (Some(input), Some(binding), Some(ordinal)) = (
            self.notifications.inputs.get(event_id),
            self.notifications.bindings.get(event_id),
            self.events.get(event_id),
        ) else {
            return;
        };
        let id = ItemId::Committed {
            cursor: super::contract::HistoryCursor::event(
                self.source.clone(),
                *ordinal,
                event_id.clone(),
            ),
            part: 0,
        };
        let Some(row) = self.items.get_mut(&id) else {
            return;
        };
        let valid = !binding.conflicted
            && *ordinal < binding.ordinal
            && input.sender == binding.audit.sender
            && input.from == binding.audit.from
            && input.seq == binding.audit.seq;
        if valid {
            row.kind = ViewItemKind::ExternalInput;
            row.label = DisplayText::new(&input.summary);
            self.notifications.bound_items.insert(id);
        } else if self.notifications.bound_items.remove(&id) {
            row.kind = ViewItemKind::Input;
            row.label = DisplayText::new("Input");
        }
    }
}

// Parsing produces a candidate only. It never grants notification attribution;
// the owning projection separately requires an explicit typed delivery binding.
fn framed_input(text: &str) -> Option<FramedInput> {
    let (header, body) = text.strip_prefix("<agent_message ")?.split_once(">\n")?;
    let body = body.strip_suffix("\n</agent_message>")?;
    let mut attrs = header;
    let from = attribute(&mut attrs, "from")?;
    let sender = attribute(&mut attrs, "from_id")?.parse().ok()?;
    let role = if attrs.starts_with("role=") {
        Some(attribute(&mut attrs, "role")?)
    } else {
        None
    };
    let kind = match attribute(&mut attrs, "kind")?.as_str() {
        "steer" => MessageKind::Steer,
        "update" => MessageKind::Update,
        _ => return None,
    };
    let seq = if attrs.starts_with("seq=") {
        Some(attribute(&mut attrs, "seq")?.parse().ok()?)
    } else {
        None
    };
    let timestamp = attribute(&mut attrs, "ts")?.parse().ok()?;
    if !attrs.is_empty() {
        return None;
    }
    let content = unescape(body)?;
    // Identity fields absent from the frame are not inferred. Nil placeholders
    // are used only to check the existing formatter's exact byte grammar.
    let message = ChannelMessage {
        id: Uuid::nil(),
        sender_id: sender,
        from: from.clone(),
        role,
        to_id: Uuid::nil(),
        content,
        kind,
        seq,
        timestamp,
    };
    if frame_message(&message) != text {
        return None;
    }
    let summary = message_summary(&message)?;
    Some(FramedInput {
        sender,
        from,
        seq,
        summary,
    })
}

fn attribute(input: &mut &str, name: &str) -> Option<String> {
    let rest = input.strip_prefix(name)?.strip_prefix("=\"")?;
    let (value, remaining) = rest.split_once('"')?;
    *input = if remaining.is_empty() {
        remaining
    } else {
        remaining.strip_prefix(' ')?
    };
    unescape(value)
}

fn unescape(text: &str) -> Option<String> {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((prefix, encoded)) = rest.split_once('&') {
        result.push_str(prefix);
        let (character, suffix) = if let Some(suffix) = encoded.strip_prefix("amp;") {
            ('&', suffix)
        } else if let Some(suffix) = encoded.strip_prefix("lt;") {
            ('<', suffix)
        } else if let Some(suffix) = encoded.strip_prefix("gt;") {
            ('>', suffix)
        } else if let Some(suffix) = encoded.strip_prefix("quot;") {
            ('"', suffix)
        } else {
            return None;
        };
        result.push(character);
        rest = suffix;
    }
    result.push_str(rest);
    Some(result)
}

#[derive(Deserialize)]
struct ProcessNotice {
    process_id: String,
    exit_code: Option<i32>,
    killed: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum WatchNotice {
    #[serde(rename = "watch_match")]
    Match {
        watch_id: String,
        process_id: String,
    },
    #[serde(rename = "watch_error")]
    Error {
        watch_id: String,
        process_id: String,
    },
}

fn message_summary(message: &ChannelMessage) -> Option<String> {
    if message.sender_id.is_nil() && message.from == "norn:process-manager" {
        let process: ProcessNotice = serde_json::from_str(&message.content).ok()?;
        let result = if process.killed {
            "killed".to_owned()
        } else {
            process.exit_code.map_or_else(
                || "no exit code".to_owned(),
                |code| format!("exit code {code}"),
            )
        };
        Some(format!(
            "Process {} finished ({result}) — {}",
            process.process_id, message.from
        ))
    } else if message.sender_id.is_nil() && message.from == "norn:watch" {
        let watch: WatchNotice = serde_json::from_str(&message.content).ok()?;
        let (watch_id, process_id, outcome) = match watch {
            WatchNotice::Match {
                watch_id,
                process_id,
            } => (watch_id, process_id, "matched"),
            WatchNotice::Error {
                watch_id,
                process_id,
            } => (watch_id, process_id, "failed"),
        };
        Some(format!(
            "Watch {watch_id} on {process_id} {outcome} — {}",
            message.from
        ))
    } else {
        Some(format!("Message from {}", message.from))
    }
}
