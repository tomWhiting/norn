use crate::provider::request::MessageRole;
use crate::rules::engine::RuleEngine;
use crate::rules::source::RuleOrigin;
use crate::rules::types::{DeliveryMode, RuleId, RuleInjection, TriggerTiming};
use crate::session::conversion::events_to_messages;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

use super::conversation_state::event_produces_prompt_message;
use super::loop_context::LoopContext;
use super::rule_wiring::{apply_rule_injections, persist_before_injection_audit};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn injection(origin: RuleOrigin, delivery: DeliveryMode) -> RuleInjection {
    RuleInjection {
        rule_id: RuleId::from("authority"),
        origin,
        delivery,
        timing: TriggerTiming::After,
        content: "body".to_owned(),
    }
}

#[tokio::test]
async fn live_and_resume_projection_match_for_each_origin_and_delivery() -> TestResult {
    for (origin, expected_role) in [
        (RuleOrigin::Operator, MessageRole::Developer),
        (RuleOrigin::Workspace, MessageRole::User),
    ] {
        for delivery in [
            DeliveryMode::SystemContextAppend,
            DeliveryMode::ContextInjection,
            DeliveryMode::MessageDelivery,
        ] {
            let store = EventStore::new();
            let mut context = LoopContext::new("base");
            context.rules = Some(RuleEngine::new(Vec::new()));
            let mut live = Vec::new();
            apply_rule_injections(
                &mut context,
                vec![injection(origin, delivery.clone())],
                &mut live,
                &store,
            )
            .await?;

            assert_eq!(live.len(), 1);
            assert_eq!(live[0].role, expected_role);
            let events = store.events();
            let [
                SessionEvent::RuleInjection {
                    origin: stored_origin,
                    delivery: stored_delivery,
                    ..
                },
            ] = events.as_slice()
            else {
                return Err(std::io::Error::other("missing durable rule event").into());
            };
            assert_eq!(*stored_origin, Some(origin));
            assert_eq!(*stored_delivery, delivery);
            assert!(event_produces_prompt_message(&events[0], true));

            let resumed = events_to_messages(&events);
            assert_eq!(resumed.len(), 1);
            assert_eq!(resumed[0].role, live[0].role);
            assert_eq!(resumed[0].content, live[0].content);

            assert_eq!(context.dynamic_context(), None);
        }
    }
    Ok(())
}

#[tokio::test]
async fn before_audit_write_always_persists_origin() -> TestResult {
    let store = EventStore::new();
    persist_before_injection_audit(
        &store,
        None,
        &[injection(
            RuleOrigin::Workspace,
            DeliveryMode::SystemContextAppend,
        )],
    )
    .await?;
    assert!(matches!(
        store.events().as_slice(),
        [SessionEvent::RuleInjection {
            origin: Some(RuleOrigin::Workspace),
            ..
        }]
    ));
    Ok(())
}

#[test]
fn legacy_originless_rules_are_all_conservative_user_messages() -> TestResult {
    let store = EventStore::new();
    for (rule_id, delivery) in [
        ("append", DeliveryMode::SystemContextAppend),
        ("context", DeliveryMode::ContextInjection),
        ("message", DeliveryMode::MessageDelivery),
    ] {
        store.append(SessionEvent::RuleInjection {
            base: EventBase::new(store.last_event_id()),
            rule_id: rule_id.to_owned(),
            origin: None,
            delivery,
            timing: TriggerTiming::After,
            content: format!("{rule_id} body"),
        })?;
    }

    let messages = events_to_messages(&store.events());
    assert_eq!(messages.len(), 3);
    assert!(
        messages
            .iter()
            .all(|message| message.role == MessageRole::User)
    );
    let events = store.events();
    assert!(event_produces_prompt_message(&events[0], true));
    assert!(event_produces_prompt_message(&events[1], true));
    assert!(event_produces_prompt_message(&events[2], true));

    Ok(())
}
