use super::*;
use crate::rules::types::{DeliveryMode, TriggerTiming};

#[tokio::test]
async fn legacy_system_append_forces_one_replay_before_v2_threading() -> TestResult {
    const LEGACY_RULE: &str = "legacy-system-append-rule";

    let server = MockServer::start().await;
    mount_response_sequence(&server, &["resp_rebound", "resp_threaded"]).await?;
    let provider = wire_provider(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    store.append(SessionEvent::RuleInjection {
        base: EventBase::new(None),
        rule_id: "legacy-rule".to_owned(),
        origin: None,
        delivery: DeliveryMode::SystemContextAppend,
        timing: TriggerTiming::After,
        content: LEGACY_RULE.to_owned(),
    })?;
    append_legacy_v1_anchor(&store, "resp_legacy")?;

    let mut loop_context = LoopContext::new("legacy");
    loop_context.install_stable_prompt_plan(three_authority_plan("product-policy"));
    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "first-task").await?);
    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "second-task").await?);

    let payloads = received_payloads(&server, 2).await?;
    assert!(
        payloads[0].get("previous_response_id").is_none(),
        "the incompatible V1 anchor must force a complete local replay",
    );
    let first_wire = serde_json::to_string(&payloads[0])?;
    assert_eq!(first_wire.matches(LEGACY_RULE).count(), 1);
    assert!(payloads[0]["input"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["role"] == "user"
                && serde_json::to_string(item).is_ok_and(|encoded| encoded.contains(LEGACY_RULE))
        })
    }));

    assert_eq!(payloads[1]["previous_response_id"], "resp_rebound");
    assert!(
        !serde_json::to_string(&payloads[1])?.contains(LEGACY_RULE),
        "the seed-bound V2 anchor must resume normal delta threading",
    );
    Ok(())
}
