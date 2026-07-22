//! Source-aware stable prompt assembly for freshly spawned children.

use super::{PromptPlan, PromptSource};

const CHILD_AGENT_POLICY: &str = "You are a sub-agent. Complete the task and stop.";

/// One optional prompt fragment selected for a child launch.
///
/// Each variant carries its provenance in the type so callers cannot attach
/// an independent provider role to the text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChildPromptFragment<'a> {
    /// Prompt compiled into a built-in Norn variant.
    BuiltinVariant(&'a str),
    /// Prompt supplied through merged variant configuration.
    ConfiguredVariant(&'a str),
    /// Profile discovered from a trusted user-level profile directory.
    OperatorProfile(&'a str),
    /// Profile discovered inside the active workspace.
    WorkspaceProfile(&'a str),
}

impl ChildPromptFragment<'_> {
    const fn source(self) -> PromptSource {
        match self {
            Self::BuiltinVariant(_) => PromptSource::BuiltinVariant,
            Self::ConfiguredVariant(_) => PromptSource::ConfiguredVariant,
            Self::OperatorProfile(_) => PromptSource::OperatorProfile,
            Self::WorkspaceProfile(_) => PromptSource::WorkspaceProfile,
        }
    }

    fn content(self) -> String {
        match self {
            Self::BuiltinVariant(content)
            | Self::ConfiguredVariant(content)
            | Self::OperatorProfile(content)
            | Self::WorkspaceProfile(content) => content.trim_end().to_owned(),
        }
    }
}

/// Build a spawned child's stable prompt without embedding its task.
///
/// The task is deliberately absent from this API. Launchers pass it once as
/// the run's ordinary User prompt, while the compiled child policy and any
/// provenance-bearing variant/profile guidance stay in the stable plan.
#[must_use]
pub(crate) fn build_child_prompt_plan(fragment: Option<ChildPromptFragment<'_>>) -> PromptPlan {
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ChildAgentPolicy, CHILD_AGENT_POLICY);
    if let Some(fragment) = fragment {
        plan.set(fragment.source(), fragment.content());
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_prompt::PromptAuthority;

    #[test]
    fn task_cannot_enter_stable_child_plan() {
        let sentinel = "USER_TASK_SENTINEL";
        let plan = build_child_prompt_plan(Some(ChildPromptFragment::BuiltinVariant(
            "builtin guidance",
        )));

        assert!(
            plan.fragments()
                .iter()
                .all(|fragment| !fragment.content().contains(sentinel))
        );
        assert_eq!(plan.fragments().len(), 2);
        assert!(
            plan.fragments()
                .iter()
                .all(|fragment| fragment.authority() == PromptAuthority::System)
        );
    }

    #[test]
    fn configured_and_workspace_guidance_remain_user_authority() {
        for fragment in [
            ChildPromptFragment::ConfiguredVariant("configured"),
            ChildPromptFragment::WorkspaceProfile("workspace"),
        ] {
            let plan = build_child_prompt_plan(Some(fragment));
            assert_eq!(plan.fragments().len(), 2);
            assert_eq!(plan.fragments()[1].authority(), PromptAuthority::User);
        }
    }

    #[test]
    fn operator_profile_guidance_is_developer_authority() {
        let plan = build_child_prompt_plan(Some(ChildPromptFragment::OperatorProfile("operator")));
        assert_eq!(plan.fragments().len(), 2);
        assert_eq!(plan.fragments()[1].authority(), PromptAuthority::Developer);
    }
}
