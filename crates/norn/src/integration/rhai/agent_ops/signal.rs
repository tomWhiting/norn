//! Rhai `signal_agent` overloads and delivery implementation.

use chrono::{DateTime, Utc};
use rhai::{Dynamic, Engine, EvalAltResult, ImmutableString};
use uuid::Uuid;

use super::super::context::{AgentHandle, NornRhaiContext, dynamic_to_json, rhai_error};
use crate::agent::registry::AgentStatus;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::tools::agent::append_message_audit;
use crate::tools::agent::coord::sender_attribution;

pub(super) fn register(engine: &mut Engine, context: &NornRhaiContext) {
    let ctx = context.clone();
    engine.register_fn(
        "signal_agent",
        move |to: AgentHandle, content: Dynamic| -> Result<u64, Box<EvalAltResult>> {
            signal_agent(&ctx, to.0, &content, MessageKind::Update)
        },
    );

    let ctx = context.clone();
    engine.register_fn(
        "signal_agent",
        move |to: AgentHandle,
              content: Dynamic,
              kind: ImmutableString|
              -> Result<u64, Box<EvalAltResult>> {
            signal_agent(&ctx, to.0, &content, parse_kind(kind.as_str())?)
        },
    );

    let ctx = context.clone();
    engine.register_fn(
        "signal_agent",
        move |to: ImmutableString, content: Dynamic| -> Result<u64, Box<EvalAltResult>> {
            let id = resolve_recipient(&ctx, to.as_str())?;
            signal_agent(&ctx, id, &content, MessageKind::Update)
        },
    );

    let ctx = context.clone();
    engine.register_fn(
        "signal_agent",
        move |to: ImmutableString,
              content: Dynamic,
              kind: ImmutableString|
              -> Result<u64, Box<EvalAltResult>> {
            let id = resolve_recipient(&ctx, to.as_str())?;
            signal_agent(&ctx, id, &content, parse_kind(kind.as_str())?)
        },
    );
}

fn parse_kind(raw: &str) -> Result<MessageKind, Box<EvalAltResult>> {
    match raw {
        "steer" => Ok(MessageKind::Steer),
        "update" => Ok(MessageKind::Update),
        other => Err(Box::new(rhai_error(format!(
            "signal_agent: unknown kind '{other}' — expected \"steer\" or \"update\""
        )))),
    }
}

fn finished_error(
    identifier: &str,
    status: AgentStatus,
    completed_at: Option<DateTime<Utc>>,
) -> EvalAltResult {
    let when = completed_at.map_or_else(|| "an unrecorded time".to_owned(), |ts| ts.to_rfc3339());
    let outcome = if status == AgentStatus::Failed {
        "failed"
    } else {
        "completed"
    };
    rhai_error(format!(
        "signal_agent: recipient already finished: agent '{identifier}' {outcome} at \
         {when} and can no longer receive messages"
    ))
}

fn resolve_recipient(ctx: &NornRhaiContext, to: &str) -> Result<Uuid, Box<EvalAltResult>> {
    if let Ok(parsed) = Uuid::parse_str(to) {
        return Ok(parsed);
    }
    let reg = ctx.registry.read();
    if let Some(entry) = reg.get_by_path(to) {
        if entry.status.is_terminal() {
            return Err(Box::new(finished_error(
                to,
                entry.status,
                entry.completed_at,
            )));
        }
        return Ok(entry.id);
    }
    if let Some(entry) = reg.get_terminal_by_path(to) {
        return Err(Box::new(finished_error(
            to,
            entry.status,
            entry.completed_at,
        )));
    }
    if let Some(tombstone) = reg.tombstone_by_path(to) {
        return Err(Box::new(finished_error(
            to,
            tombstone.status,
            Some(tombstone.completed_at),
        )));
    }
    Err(Box::new(rhai_error(format!(
        "signal_agent: unknown recipient '{to}'"
    ))))
}

fn signal_agent(
    ctx: &NornRhaiContext,
    to_id: Uuid,
    content: &Dynamic,
    kind: MessageKind,
) -> Result<u64, Box<EvalAltResult>> {
    let json = dynamic_to_json(content)?;
    let body = match json {
        serde_json::Value::String(s) => s,
        other => serde_json::to_string(&other).map_err(|e| {
            Box::new(rhai_error(format!(
                "signal_agent: could not serialize content: {e}"
            )))
        })?,
    };
    let (from_label, from_role, to_label) = {
        let reg = ctx.registry.read();
        if let Some(entry) = reg.get(to_id) {
            if entry.status.is_terminal() {
                return Err(Box::new(finished_error(
                    &to_id.to_string(),
                    entry.status,
                    entry.completed_at,
                )));
            }
        } else if let Some(tombstone) = reg.tombstone(to_id) {
            return Err(Box::new(finished_error(
                &to_id.to_string(),
                tombstone.status,
                Some(tombstone.completed_at),
            )));
        }
        let (label, role) = sender_attribution(&reg, ctx.agent_id, None);
        let to_label = reg
            .get(to_id)
            .map_or_else(|| to_id.to_string(), |entry| entry.path);
        (label, role, to_label)
    };
    let message_id = Uuid::new_v4();
    let sent_at = Utc::now();
    let msg = ChannelMessage {
        id: message_id,
        sender_id: ctx.agent_id,
        from: from_label.clone(),
        role: from_role,
        to_id,
        content: body.clone(),
        kind,
        seq: None,
        timestamp: sent_at,
    };
    let seq = ctx
        .router
        .try_deliver(to_id, msg)
        .map_err(|e| Box::new(rhai_error(format!("signal_agent: {e}"))))?;
    append_message_audit(
        &ctx.event_store,
        &crate::provider::agent_event::AgentMessageLifecycle::Sent {
            message_id,
            from_id: ctx.agent_id,
            from: from_label,
            to_id,
            to: to_label,
            kind,
            seq,
            content: body,
            sent_at,
        },
    )
    .map_err(|error| {
        Box::new(rhai_error(format!(
            "signal_agent: message {message_id} WAS delivered (seq {seq}); do \
             NOT resend it. Persisting the durable Sent audit failed: {error}",
        )))
    })?;
    Ok(seq)
}
