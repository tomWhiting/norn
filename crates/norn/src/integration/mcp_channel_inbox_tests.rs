//! Admission lifetime and cancellation tests independent of provider calls.

use futures_util::FutureExt;
use uuid::Uuid;

use super::{McpChannelHost, McpChannelInbox};
use crate::integration::mcp_channel_source::ChannelSource;
use crate::integration::{
    McpChannelError, McpChannelLimits, McpChannelOverflow, McpChannelPolicy, McpChannelRefusal,
};

fn source(
    host: &McpChannelHost,
    name: &str,
    generation: u64,
    policy: McpChannelPolicy,
) -> Result<ChannelSource, McpChannelError> {
    let source = host
        .attachment(policy, McpChannelOverflow::RejectNew)
        .bind(name.to_owned(), generation)?;
    source.negotiated()?;
    source.activate()?;
    Ok(source)
}

fn send(source: &ChannelSource, content: &str) {
    source.receive(serde_json::json!({"content":content,"meta":{"chat_id":"table-7"}}));
}

#[tokio::test]
async fn wake_only_claim_preserves_earlier_next_turn_messages()
-> Result<(), Box<dyn std::error::Error>> {
    let mut inbox = McpChannelInbox::new(Uuid::new_v4(), McpChannelLimits::new(2, 4096)?);
    let host = inbox.host();
    let next = source(&host, "quiet", 1, McpChannelPolicy::NextTurn)?;
    let wake = source(&host, "urgent", 2, McpChannelPolicy::Wake)?;
    send(&next, "next");
    send(&wake, "wake");
    let claim = inbox.try_claim_wake()?.ok_or("missing Wake claim")?;
    assert_eq!(claim.message().content(), "wake");
    assert_eq!(host.status().retained_messages, 2);
    claim.consume()?;
    assert!(inbox.try_claim_wake()?.is_none());
    let next = inbox.try_claim()?.ok_or("missing NextTurn claim")?;
    assert_eq!(next.message().content(), "next");
    next.consume()?;
    Ok(())
}

#[tokio::test]
async fn claims_and_cancelled_waits_never_release_retained_quota()
-> Result<(), Box<dyn std::error::Error>> {
    let mut inbox = McpChannelInbox::new(Uuid::new_v4(), McpChannelLimits::new(1, 1000)?);
    let host = inbox.host();
    let source = source(&host, "fixture", 1, McpChannelPolicy::Wake)?;
    assert!(inbox.claim().now_or_never().is_none());
    send(&source, "first");
    let claim = inbox.claim().await?;
    let id = claim.message().id();
    let charged = host.status().retained_bytes;
    send(&source, "second");
    assert_eq!(host.status().retained_messages, 1);
    assert_eq!(host.status().retained_bytes, charged);
    assert_eq!(
        host.status().last_rejection.map(|r| r.reason),
        Some(McpChannelRefusal::FullCount)
    );
    drop(claim);
    assert_eq!(host.status().retained_bytes, charged);
    let again = inbox.claim().await?;
    assert_eq!(again.message().id(), id);
    again.consume()?;
    assert_eq!(host.status().retained_messages, 0);
    assert_eq!(host.status().retained_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn staged_and_held_input_stays_bounded_and_cannot_wake()
-> Result<(), Box<dyn std::error::Error>> {
    let mut inbox = McpChannelInbox::new(Uuid::new_v4(), McpChannelLimits::new(1, 1000)?);
    let host = inbox.host();
    let source = host
        .attachment(McpChannelPolicy::Hold, McpChannelOverflow::RejectNew)
        .bind("fixture".to_owned(), 1)?;
    send(&source, "held");
    assert_eq!(host.status().retained_messages, 1);
    assert!(inbox.try_claim()?.is_none());
    assert!(inbox.wake_ready().now_or_never().is_none());
    source.negotiated()?;
    source.activate()?;
    assert!(inbox.try_claim()?.is_none());
    let held = inbox.held_message_ids();
    assert_eq!(held.len(), 1);
    let id = *held.first().ok_or("held id missing")?;
    send(&source, "overflow");
    assert_eq!(
        host.status().last_rejection.map(|r| r.reason),
        Some(McpChannelRefusal::FullCount)
    );
    host.release(id, McpChannelPolicy::NextTurn)?;
    assert!(inbox.wake_ready().now_or_never().is_none());
    let claim = inbox.claim().await?;
    assert_eq!(claim.message().id(), id);
    claim.consume()?;
    send(&source, "deny");
    let deny = *inbox.held_message_ids().first().ok_or("deny id missing")?;
    host.deny(deny)?;
    assert_eq!(host.status().retained_messages, 0);
    Ok(())
}

#[tokio::test]
async fn replacement_stages_without_retiring_current_then_fences_new_old_events()
-> Result<(), Box<dyn std::error::Error>> {
    let recipient = Uuid::new_v4();
    let mut inbox = McpChannelInbox::new(recipient, McpChannelLimits::new(10, 1000)?);
    let host = inbox.host();
    let first = source(&host, "fixture", 1, McpChannelPolicy::Wake)?;
    let replacement = host
        .attachment(McpChannelPolicy::Wake, McpChannelOverflow::RejectNew)
        .bind("fixture".to_owned(), 2)?;
    send(&first, "old admitted");
    send(&replacement, "new staged");
    replacement.negotiated()?;
    let old = inbox.claim().await?;
    assert_eq!(old.message().generation(), 1);
    assert_eq!(old.message().recipient_id(), recipient);
    old.consume()?;
    assert!(inbox.try_claim()?.is_none());
    replacement.activate()?;
    send(&first, "retired refused");
    assert_eq!(
        host.status().last_rejection.map(|r| r.reason),
        Some(McpChannelRefusal::Retired)
    );
    let current = inbox.claim().await?;
    assert_eq!(current.message().generation(), 2);
    assert_eq!(current.message().content(), "new staged");
    current.consume()?;
    Ok(())
}

#[tokio::test]
async fn retiring_active_source_preserves_admitted_messages_and_drop_rejects_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let mut inbox = McpChannelInbox::new(Uuid::new_v4(), McpChannelLimits::new(10, 1000)?);
    let host = inbox.host();
    let active = source(&host, "fixture", 1, McpChannelPolicy::Wake)?;
    send(&active, "already admitted");
    active.retire()?;
    drop(active);
    let claim = inbox.claim().await?;
    assert_eq!(claim.message().content(), "already admitted");
    claim.consume()?;
    let candidate = host
        .attachment(McpChannelPolicy::Wake, McpChannelOverflow::RejectNew)
        .bind("fixture".to_owned(), 2)?;
    send(&candidate, "never activated");
    drop(candidate);
    assert_eq!(host.status().retained_messages, 0);
    assert_eq!(
        host.status().last_rejection.map(|r| r.reason),
        Some(McpChannelRefusal::CandidateAbandoned)
    );
    Ok(())
}

#[tokio::test]
async fn byte_accounting_is_shared_across_sources_and_closed_receiver_refuses()
-> Result<(), Box<dyn std::error::Error>> {
    let mut inbox = McpChannelInbox::new(Uuid::new_v4(), McpChannelLimits::new(10, 4)?);
    let host = inbox.host();
    let first = source(&host, "a", 1, McpChannelPolicy::Wake)?;
    let second = source(&host, "b", 2, McpChannelPolicy::Wake)?;
    first.receive(serde_json::json!({"content":"é"}));
    second.receive(serde_json::json!({"content":"x"}));
    assert_eq!(host.status().retained_bytes, 3);
    assert_eq!(
        host.status().last_rejection.map(|r| r.reason),
        Some(McpChannelRefusal::FullBytes)
    );
    let claim = inbox.claim().await?;
    claim.consume()?;
    drop(inbox);
    second.receive(serde_json::json!({"content":"x"}));
    assert_eq!(
        host.status().last_rejection.map(|r| r.reason),
        Some(McpChannelRefusal::Closed)
    );
    Ok(())
}
