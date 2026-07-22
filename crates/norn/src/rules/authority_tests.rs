use crate::provider::request::MessageRole;

use super::engine::RuleEngine;
use super::projection::{RuleConversationProjection, project_rule_injection, project_sourced_rule};
use super::source::RuleOrigin;
use super::types::{DeliveryMode, Rule, RuleId, RuntimeEvent, TriggerCondition, TriggerTiming};

fn rule(id: &str) -> Rule {
    Rule {
        id: RuleId::from(id),
        name: id.to_owned(),
        triggers: vec![TriggerCondition::ToolInvocation {
            tool_name: "read".to_owned(),
        }],
        delivery: DeliveryMode::MessageDelivery,
        timing: TriggerTiming::After,
        body: format!("{id} body"),
        shell_source: None,
    }
}

fn event() -> RuntimeEvent {
    RuntimeEvent::ToolInvoked {
        tool_name: "read".to_owned(),
        arguments: None,
    }
}

#[test]
fn sourced_projection_matrix_derives_role_only_from_origin() {
    let cases = [
        (RuleOrigin::Operator, MessageRole::Developer),
        (RuleOrigin::Workspace, MessageRole::User),
    ];
    let deliveries = [
        (DeliveryMode::SystemContextAppend, "body"),
        (DeliveryMode::ContextInjection, "[Context: rule] body"),
        (DeliveryMode::MessageDelivery, "[Rule: rule] body"),
    ];

    for (origin, expected_role) in cases {
        for (delivery, expected_content) in &deliveries {
            let projection = project_sourced_rule(origin, delivery, "rule", "body");
            assert_eq!(projection.role, expected_role);
            assert_eq!(projection.content, *expected_content);
        }
    }
}

#[test]
fn legacy_projection_is_conservatively_user_for_every_delivery() {
    let append = project_rule_injection(
        None,
        &DeliveryMode::SystemContextAppend,
        "legacy",
        "append body",
    );
    assert_eq!(
        append,
        RuleConversationProjection {
            role: MessageRole::User,
            content: "append body".to_owned(),
        }
    );

    for (delivery, expected) in [
        (
            DeliveryMode::ContextInjection,
            "[Context: legacy] legacy body",
        ),
        (DeliveryMode::MessageDelivery, "[Rule: legacy] legacy body"),
    ] {
        assert_eq!(
            project_rule_injection(None, &delivery, "legacy", "legacy body"),
            RuleConversationProjection {
                role: MessageRole::User,
                content: expected.to_owned(),
            }
        );
    }
}

#[test]
fn discovery_origin_uses_the_actual_directory_index() {
    let workspace_indexes = [2, 5];
    assert_eq!(
        RuleOrigin::from_discovery_directory(0, &workspace_indexes),
        RuleOrigin::Operator
    );
    assert_eq!(
        RuleOrigin::from_discovery_directory(2, &workspace_indexes),
        RuleOrigin::Workspace
    );
    assert_eq!(
        RuleOrigin::from_discovery_directory(5, &workspace_indexes),
        RuleOrigin::Workspace
    );
}

#[tokio::test]
async fn programmatic_rules_default_operator_and_workspace_api_is_explicit() {
    let mut engine = RuleEngine::new(vec![rule("constructor")]);
    engine.add_rule(rule("public-add"));
    engine.add_workspace_rule(rule("workspace-add"));

    let injections = engine.process_event(&event()).await;
    let observed: Vec<_> = injections
        .iter()
        .map(|injection| (injection.rule_id.as_str(), injection.origin))
        .collect();
    assert_eq!(
        observed,
        vec![
            ("constructor", RuleOrigin::Operator),
            ("public-add", RuleOrigin::Operator),
            ("workspace-add", RuleOrigin::Workspace),
        ]
    );
}
