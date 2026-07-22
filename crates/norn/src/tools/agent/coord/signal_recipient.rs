//! Recipient resolution and messaging-scope checks for `signal_agent`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agent::child_policy::MessagingScope;
use crate::agent::registry::AgentStatus;
use crate::error::ToolError;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::traits::ToolOutput;
use crate::tools::agent::infra::{AgentToolInfra, ResolvedAgent, resolve_agent};

/// Resolved recipient identity, genealogy, and terminal state.
pub(super) struct Recipient {
    pub(super) id: Uuid,
    pub(super) label: String,
    parent_id: Option<Uuid>,
    pub(super) finished: Option<(AgentStatus, Option<DateTime<Utc>>)>,
    unregistered_root: bool,
}

enum ParentResolution {
    Resolved(Recipient),
    Failure(ToolOutput),
}

pub(super) fn resolve_recipient(
    infra: &AgentToolInfra,
    to: &str,
) -> Result<Result<Recipient, ToolOutput>, ToolError> {
    if to == "parent" {
        return Ok(match resolve_parent(infra, to) {
            ParentResolution::Resolved(recipient) => Ok(recipient),
            ParentResolution::Failure(output) => Err(output),
        });
    }

    let recipient = match resolve_agent(&infra.registry, to)? {
        ResolvedAgent::Live(entry) => {
            let finished = entry
                .status
                .is_terminal()
                .then_some((entry.status, entry.completed_at));
            Recipient {
                id: entry.id,
                label: entry.path,
                parent_id: entry.parent_id,
                finished,
                unregistered_root: false,
            }
        }
        ResolvedAgent::Reclaimed(tombstone) => Recipient {
            id: tombstone.id,
            label: tombstone.path,
            parent_id: tombstone.parent_id,
            finished: Some((tombstone.status, Some(tombstone.completed_at))),
            unregistered_root: false,
        },
    };
    Ok(Ok(recipient))
}

fn resolve_parent(infra: &AgentToolInfra, to: &str) -> ParentResolution {
    let Some(parent_id) = infra.parent_id else {
        return ParentResolution::Failure(ToolOutput::failure_with_content(
            serde_json::json!({ "delivered": false }),
            ToolErrorPayload::new(
                ToolErrorKind::NotFound,
                "this agent has no parent — \"parent\" does not resolve for a root \
                 agent. Address a recipient by registry path or UUID instead."
                    .to_owned(),
            )
            .with_detail(serde_json::json!({ "to": to })),
        ));
    };
    let registry = infra.registry.read();
    if let Some(entry) = registry.get(parent_id) {
        let finished = entry
            .status
            .is_terminal()
            .then_some((entry.status, entry.completed_at));
        return ParentResolution::Resolved(Recipient {
            id: entry.id,
            label: entry.path,
            parent_id: entry.parent_id,
            finished,
            unregistered_root: false,
        });
    }
    if let Some(tombstone) = registry.tombstone(parent_id) {
        return ParentResolution::Resolved(Recipient {
            id: tombstone.id,
            label: tombstone.path,
            parent_id: tombstone.parent_id,
            finished: Some((tombstone.status, Some(tombstone.completed_at))),
            unregistered_root: false,
        });
    }
    ParentResolution::Resolved(Recipient {
        id: parent_id,
        label: "root".to_owned(),
        parent_id: None,
        finished: None,
        unregistered_root: true,
    })
}

pub(super) fn scope_denial(
    infra: &AgentToolInfra,
    recipient: &Recipient,
    to: &str,
) -> Result<Option<ToolOutput>, ToolError> {
    let denial = |scope: &str, reason: String| {
        Some(ToolOutput::failure_with_content(
            serde_json::json!({
                "delivered": false,
                "to": recipient.id.to_string(),
                "scope": scope,
            }),
            ToolErrorPayload::new(ToolErrorKind::PermissionDenied, reason)
                .with_detail(serde_json::json!({ "to": to })),
        ))
    };

    let Some(sender_parent) = infra.parent_id else {
        if recipient.parent_id == Some(infra.agent_id) {
            return Ok(None);
        }
        return Ok(denial(
            "root",
            format!(
                "out of scope: a root agent may message only its own children; \
                 '{to}' is not a child of this agent."
            ),
        ));
    };

    let Some(policy) = infra.grant.as_ref().map(|grant| &grant.policy) else {
        return Err(ToolError::ExecutionFailed {
            reason: "signal_agent: this agent has a parent but no granted ChildPolicy on \
                     its AgentToolInfra — the spawning runtime must stamp the policy from \
                     its CoordinationEnvelope at launch. This is a harness configuration \
                     error, not a model error."
                .to_owned(),
        });
    };
    let allowed = match policy.messaging {
        MessagingScope::None => {
            return Ok(denial(
                "none",
                "signal_agent is not available under this agent's messaging scope \
                 (\"none\") — the spawning parent granted no messaging capability."
                    .to_owned(),
            ));
        }
        MessagingScope::ParentOnly => recipient.id == sender_parent,
        MessagingScope::SiblingsAndParent => {
            recipient.id == sender_parent || recipient.parent_id == Some(sender_parent)
        }
    };
    if allowed {
        return Ok(None);
    }
    let (scope, description) = match policy.messaging {
        MessagingScope::ParentOnly => ("parent_only", "it may message only its parent"),
        MessagingScope::SiblingsAndParent => (
            "siblings_and_parent",
            "it may message its siblings (children of the same parent) and its parent",
        ),
        MessagingScope::None => ("none", "messaging is not granted"),
    };
    Ok(denial(
        scope,
        format!(
            "out of scope: this agent's messaging scope is \"{scope}\" — {description}; \
             '{to}' is neither. Escalation crosses one audited hop at a time: route the \
             message through your parent instead."
        ),
    ))
}

pub(super) fn finished_failure(
    recipient: &Recipient,
    to: &str,
    status: AgentStatus,
    completed_at: Option<DateTime<Utc>>,
) -> ToolOutput {
    let when = completed_at.map_or_else(|| "an unrecorded time".to_owned(), |ts| ts.to_rfc3339());
    let outcome = match status {
        AgentStatus::Failed => "failed",
        AgentStatus::Closed => "closed",
        _ => "completed",
    };
    ToolOutput::failure_with_content(
        serde_json::json!({
            "delivered": false,
            "to": recipient.id.to_string(),
            "recipient_status": status,
            "completed_at": completed_at.map(|ts| ts.to_rfc3339()),
        }),
        ToolErrorPayload::new(
            ToolErrorKind::NotFound,
            format!(
                "recipient already finished: agent '{to}' {outcome} at {when} and can \
                 no longer receive messages. Its run is over — read its delivered \
                 result instead of messaging it."
            ),
        )
        .with_detail(serde_json::json!({ "to": to })),
    )
}

pub(super) fn terminal_route_failure(
    infra: &AgentToolInfra,
    recipient: &Recipient,
    to: &str,
) -> Option<ToolOutput> {
    if recipient.unregistered_root {
        return None;
    }

    let registry = infra.registry.read();
    if let Some(entry) = registry.get(recipient.id) {
        if entry.status.is_terminal() {
            return Some(finished_failure(
                recipient,
                to,
                entry.status,
                entry.completed_at,
            ));
        }
        return None;
    }
    registry.tombstone(recipient.id).map(|tombstone| {
        finished_failure(
            recipient,
            to,
            tombstone.status,
            Some(tombstone.completed_at),
        )
    })
}
