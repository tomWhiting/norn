//! Canonical provider-message projection for fired rules.

use crate::provider::request::MessageRole;
use crate::rules::source::RuleOrigin;
use crate::rules::types::DeliveryMode;

/// Conversation projection for one durable rule firing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuleConversationProjection {
    /// Provider-neutral role derived from provenance.
    pub(crate) role: MessageRole,
    /// Delivery-specific display content.
    pub(crate) content: String,
}

/// Project a new rule firing to exactly one conversation message.
#[must_use]
pub(crate) fn project_sourced_rule(
    origin: RuleOrigin,
    delivery: &DeliveryMode,
    rule_id: &str,
    content: &str,
) -> RuleConversationProjection {
    RuleConversationProjection {
        role: origin.prompt_source().authority().into(),
        content: format_content(delivery, rule_id, content),
    }
}

/// Project a fired rule without allowing its delivery mode to select authority.
///
/// New events always carry `Some(origin)` and therefore always produce one
/// conversation message. Missing provenance is accepted only for readable
/// pre-D8 rows and projects conservatively to User: an unknown historical
/// source can never be guessed upward into Developer or System authority.
#[must_use]
pub(crate) fn project_rule_injection(
    origin: Option<RuleOrigin>,
    delivery: &DeliveryMode,
    rule_id: &str,
    content: &str,
) -> RuleConversationProjection {
    RuleConversationProjection {
        role: origin.map_or(MessageRole::User, |source| {
            source.prompt_source().authority().into()
        }),
        content: format_content(delivery, rule_id, content),
    }
}

fn format_content(delivery: &DeliveryMode, rule_id: &str, content: &str) -> String {
    match delivery {
        DeliveryMode::SystemContextAppend => content.to_owned(),
        DeliveryMode::ContextInjection => format!("[Context: {rule_id}] {content}"),
        DeliveryMode::MessageDelivery => format!("[Rule: {rule_id}] {content}"),
    }
}
