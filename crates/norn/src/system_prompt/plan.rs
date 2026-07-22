//! Ordered stable prompt fragments with source-derived authority.

use super::authority::{PromptAuthority, PromptSource};
use crate::provider::request::{Message, ToolCallCaller};

/// One exact-content fragment in a stable prompt plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptFragment {
    source: PromptSource,
    content: String,
}

impl PromptFragment {
    /// Construct a fragment from its provenance and exact content.
    #[must_use]
    pub fn new(source: PromptSource, content: impl Into<String>) -> Self {
        Self {
            source,
            content: content.into(),
        }
    }

    /// Provenance that determines this fragment's authority.
    #[must_use]
    pub const fn source(&self) -> PromptSource {
        self.source
    }

    /// Authority derived from [`Self::source`].
    #[must_use]
    pub const fn authority(&self) -> PromptAuthority {
        self.source.authority()
    }

    /// Exact fragment bytes as UTF-8 text.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// Ordered, source-addressable stable prompt plan.
///
/// Each source has at most one fragment. [`Self::set`] replaces that source
/// in place, removes it when the new content is empty, and otherwise inserts
/// it at the source's canonical position.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PromptPlan {
    fragments: Vec<PromptFragment>,
}

impl PromptPlan {
    /// Construct an empty plan.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fragments: Vec::new(),
        }
    }

    /// Insert, replace, or remove one source's exact content.
    pub fn set(&mut self, source: PromptSource, content: impl Into<String>) {
        let content = content.into();
        self.fragments.retain(|fragment| fragment.source != source);
        if content.is_empty() {
            return;
        }

        let insertion = self
            .fragments
            .iter()
            .position(|fragment| fragment.source.stable_order() > source.stable_order())
            .unwrap_or(self.fragments.len());
        self.fragments
            .insert(insertion, PromptFragment::new(source, content));
    }

    /// Remove a source from the plan.
    pub fn remove(&mut self, source: PromptSource) {
        self.fragments.retain(|fragment| fragment.source != source);
    }

    /// Ordered fragments used to materialize the stable provider prefix.
    #[must_use]
    pub fn fragments(&self) -> &[PromptFragment] {
        &self.fragments
    }

    /// Whether the plan carries no fragments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// Compatibility rendering that joins every fragment without changing
    /// its bytes.
    #[must_use]
    pub fn flattened_content(&self) -> String {
        self.fragments
            .iter()
            .map(PromptFragment::content)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Materialize the ordered fragments as provider-neutral messages.
    #[must_use]
    pub fn materialize_messages(&self) -> Vec<Message> {
        self.fragments
            .iter()
            .map(|fragment| Message {
                response_items: Vec::new(),
                role: fragment.authority().into(),
                content: Some(fragment.content.clone()),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: ToolCallCaller::Absent,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_orders_replaces_and_removes_by_source() {
        let mut plan = PromptPlan::new();
        plan.set(PromptSource::ProjectContextFile, "project");
        plan.set(PromptSource::ProductPolicy, "product");
        plan.set(PromptSource::UserContextFile, "user-home");
        plan.set(PromptSource::UserContextFile, "user-home-v2");
        plan.set(PromptSource::SkillCatalogPolicy, "");

        let sources = plan
            .fragments()
            .iter()
            .map(PromptFragment::source)
            .collect::<Vec<_>>();
        assert_eq!(
            sources,
            [
                PromptSource::ProductPolicy,
                PromptSource::UserContextFile,
                PromptSource::ProjectContextFile,
            ]
        );
        assert_eq!(
            plan.flattened_content(),
            "product\n\nuser-home-v2\n\nproject"
        );
    }

    #[test]
    fn source_order_is_total_and_independent_of_insertion_or_replacement() {
        let sources = [
            PromptSource::WorkspaceRule,
            PromptSource::WorkspaceProfile,
            PromptSource::OperatorRule,
            PromptSource::OperatorProfile,
        ];
        let mut forward = PromptPlan::new();
        let mut reverse = PromptPlan::new();
        for source in sources {
            forward.set(source, source.as_str());
        }
        for source in sources.into_iter().rev() {
            reverse.set(source, source.as_str());
        }

        let ordered_sources = |plan: &PromptPlan| {
            plan.fragments()
                .iter()
                .map(PromptFragment::source)
                .collect::<Vec<_>>()
        };
        assert_eq!(ordered_sources(&forward), ordered_sources(&reverse));

        forward.set(PromptSource::OperatorRule, "replacement");
        assert_eq!(ordered_sources(&forward), ordered_sources(&reverse));
    }

    #[test]
    fn materialization_never_emits_system_after_developer_or_user() {
        let sources = [
            PromptSource::UserRequest,
            PromptSource::WorkspaceRule,
            PromptSource::WorkspaceSkillCatalog,
            PromptSource::ProjectContextFile,
            PromptSource::ConfiguredVariant,
            PromptSource::WorkspaceProfile,
            PromptSource::OperatorRule,
            PromptSource::OperatorSkillCatalog,
            PromptSource::UserContextFile,
            PromptSource::OperatorOverride,
            PromptSource::OperatorProfile,
            PromptSource::SkillCatalogPolicy,
            PromptSource::BuiltinVariant,
            PromptSource::ForkAgentPolicy,
            PromptSource::ChildAgentPolicy,
            PromptSource::EmbedderPolicy,
            PromptSource::ProductPolicy,
        ];
        let mut plan = PromptPlan::new();
        for source in sources {
            plan.set(source, source.as_str());
        }

        let ordered_sources = plan
            .fragments()
            .iter()
            .map(PromptFragment::source)
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_sources,
            [
                PromptSource::ProductPolicy,
                PromptSource::EmbedderPolicy,
                PromptSource::ChildAgentPolicy,
                PromptSource::ForkAgentPolicy,
                PromptSource::BuiltinVariant,
                PromptSource::SkillCatalogPolicy,
                PromptSource::OperatorProfile,
                PromptSource::OperatorOverride,
                PromptSource::UserContextFile,
                PromptSource::OperatorSkillCatalog,
                PromptSource::OperatorRule,
                PromptSource::WorkspaceProfile,
                PromptSource::ConfiguredVariant,
                PromptSource::ProjectContextFile,
                PromptSource::WorkspaceSkillCatalog,
                PromptSource::WorkspaceRule,
                PromptSource::UserRequest,
            ],
        );

        let roles = plan
            .materialize_messages()
            .into_iter()
            .map(|message| message.role)
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            [
                crate::provider::request::MessageRole::System,
                crate::provider::request::MessageRole::System,
                crate::provider::request::MessageRole::System,
                crate::provider::request::MessageRole::System,
                crate::provider::request::MessageRole::System,
                crate::provider::request::MessageRole::System,
                crate::provider::request::MessageRole::Developer,
                crate::provider::request::MessageRole::Developer,
                crate::provider::request::MessageRole::Developer,
                crate::provider::request::MessageRole::Developer,
                crate::provider::request::MessageRole::Developer,
                crate::provider::request::MessageRole::User,
                crate::provider::request::MessageRole::User,
                crate::provider::request::MessageRole::User,
                crate::provider::request::MessageRole::User,
                crate::provider::request::MessageRole::User,
                crate::provider::request::MessageRole::User,
            ]
        );
    }
}
