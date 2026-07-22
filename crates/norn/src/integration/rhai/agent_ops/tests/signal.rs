use uuid::Uuid;

use super::support::{TestResult, build_context, require, require_error};
use crate::agent::registry::AgentRegistry;
use crate::integration::rhai::context::build_norn_engine;
use crate::r#loop::inbound::inbound_channel;
use crate::provider::agent_event::AGENT_MESSAGE_SENT_EVENT_TYPE;
use crate::session::events::SessionEvent;

#[tokio::test(flavor = "multi_thread")]
async fn signal_agent_delivers_via_router() -> TestResult {
    let ctx = build_context();
    let target_id = Uuid::new_v4();
    let (tx, mut rx) = inbound_channel(8);
    ctx.router.register(target_id, tx);

    let engine = build_norn_engine(&ctx);
    let script = format!(r#"signal_agent("{target_id}", #{{ kind: "hello", text: "hi" }})"#);
    let seq = engine.eval::<u64>(&script)?;
    assert_eq!(seq, 1, "router-minted sequence is returned to the script");

    let messages = rx.drain();
    assert_eq!(messages.len(), 1, "exactly one message delivered");
    let message = require(messages.first(), "one delivered message must be present")?;
    assert_eq!(message.sender_id, ctx.agent_id);
    assert_eq!(message.from, "root");
    assert_eq!(message.seq, Some(1));
    let content: serde_json::Value = serde_json::from_str(&message.content)?;
    assert_eq!(content["kind"], "hello");
    assert_eq!(content["text"], "hi");

    let events = ctx.event_store.events();
    assert_eq!(events.len(), 1);
    let SessionEvent::Custom {
        event_type, data, ..
    } = require(events.first(), "one audit event must be present")?
    else {
        return Err(std::io::Error::other("expected Sent audit event").into());
    };
    assert_eq!(event_type, AGENT_MESSAGE_SENT_EVENT_TYPE);
    assert_eq!(data["seq"], 1);
    assert_eq!(data["to_id"], target_id.to_string());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_agent_to_unrouted_recipient_is_a_script_error() -> TestResult {
    let ctx = build_context();
    let target_id = Uuid::new_v4();
    let engine = build_norn_engine(&ctx);
    let error = require_error(
        engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "hello")"#)),
        "an unrouted recipient must fail",
    )?;
    assert!(error.to_string().contains("no live inbound route"));
    assert!(ctx.event_store.events().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_agent_kind_parameter_controls_delivery_kind() -> TestResult {
    use crate::r#loop::inbound::MessageKind;

    let ctx = build_context();
    let target_id = Uuid::new_v4();
    let (tx, mut rx) = inbound_channel(8);
    ctx.router.register(target_id, tx);
    let engine = build_norn_engine(&ctx);

    let seq = engine.eval::<u64>(&format!(
        r#"signal_agent("{target_id}", "act now", "steer")"#
    ))?;
    assert_eq!(seq, 1);
    let messages = rx.drain();
    assert_eq!(
        require(messages.first(), "steer delivery must be present")?.kind,
        MessageKind::Steer,
    );
    let events = ctx.event_store.events();
    let SessionEvent::Custom { data, .. } =
        require(events.first(), "steer audit event must be present")?
    else {
        return Err(std::io::Error::other("expected Sent audit event").into());
    };
    assert_eq!(data["kind"], "steer");

    let seq = engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "fyi")"#))?;
    assert_eq!(seq, 2);
    let updates = rx.drain();
    assert_eq!(
        require(updates.first(), "update delivery must be present")?.kind,
        MessageKind::Update,
    );

    let error = require_error(
        engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "x", "shout")"#)),
        "an unknown message kind must fail",
    )?;
    assert!(error.to_string().contains("unknown kind"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_agent_to_finished_recipient_is_honest_script_error() -> TestResult {
    let ctx = build_context();
    let guard = AgentRegistry::reserve(
        &ctx.registry,
        "/done-child".to_owned(),
        "worker".to_owned(),
        "claude".to_owned(),
        Some(ctx.agent_id),
        ctx.child_policy.grant_for_child(None)?,
        Some(&ctx.child_policy),
    )?;
    let child_id = guard.id();
    guard.confirm()?;
    ctx.registry.write().mark_completed(child_id)?;

    let engine = build_norn_engine(&ctx);
    for identifier in ["/done-child".to_owned(), child_id.to_string()] {
        let error = require_error(
            engine.eval::<u64>(&format!(r#"signal_agent("{identifier}", "hi")"#)),
            "a finished recipient must fail",
        )?;
        let message = error.to_string();
        assert!(message.contains("already finished") && message.contains("completed at"));
    }
    assert!(ctx.registry.write().remove_terminal(child_id));
    let error = require_error(
        engine.eval::<u64>(&format!(r#"signal_agent("{child_id}", "hi")"#)),
        "a tombstoned recipient must fail",
    )?;
    assert!(error.to_string().contains("already finished"));
    assert!(ctx.event_store.events().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_agent_full_channel_is_a_script_error() -> TestResult {
    let ctx = build_context();
    let target_id = Uuid::new_v4();
    let (tx, mut rx) = inbound_channel(1);
    ctx.router.register(target_id, tx);
    let engine = build_norn_engine(&ctx);

    engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "fits")"#))?;
    let error = require_error(
        engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "overflow")"#)),
        "an exhausted channel must fail",
    )?;
    assert!(error.to_string().contains("channel full"));
    assert_eq!(ctx.event_store.events().len(), 1);

    assert_eq!(rx.drain().len(), 1);
    let seq = engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "retry")"#))?;
    assert_eq!(seq, 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_agent_attributes_registered_and_reclaimed_hosts() -> TestResult {
    let mut ctx = build_context();
    let guard = AgentRegistry::reserve(
        &ctx.registry,
        "/host".to_owned(),
        "orchestrator".to_owned(),
        "claude".to_owned(),
        None,
        ctx.child_policy.clone(),
        None,
    )?;
    let host_id = guard.id();
    guard.confirm()?;
    ctx.agent_id = host_id;

    let target_id = Uuid::new_v4();
    let (tx, mut rx) = inbound_channel(8);
    ctx.router.register(target_id, tx);
    let engine = build_norn_engine(&ctx);
    engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "hello")"#))?;
    let messages = rx.drain();
    let registered = require(messages.first(), "registered-host delivery must be present")?;
    assert_eq!(registered.from, "/host");
    assert_eq!(registered.role.as_deref(), Some("orchestrator"));

    ctx.registry.write().mark_completed(host_id)?;
    assert!(ctx.registry.write().remove_terminal(host_id));
    let engine = build_norn_engine(&ctx);
    engine.eval::<u64>(&format!(r#"signal_agent("{target_id}", "late note")"#))?;
    let messages = rx.drain();
    let reclaimed = require(messages.first(), "reclaimed-host delivery must be present")?;
    assert_eq!(reclaimed.from, "/host");
    assert!(reclaimed.role.is_none());
    Ok(())
}
