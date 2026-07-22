use std::fmt::Write as _;

use crate::agent::fork::{
    ForkIdentity, ForkRequirement, ParentPromptPlan, ParentSystemInstruction, build_fork_preamble,
};
use crate::system_prompt::{PromptPlan, PromptSource};
use crate::tool::context::ToolContext;

/// Prompt material assembled for one fork launch.
pub(super) struct ForkPrompt {
    /// Identity-free plan inherited from the parent and published for the next
    /// fork generation.
    pub(super) parent_plan: PromptPlan,
    /// Parent plan plus this fork's one current identity/policy fragment.
    pub(super) active_plan: PromptPlan,
    /// Human-authored task passed once as the ordinary User prompt.
    pub(super) user_task: String,
}

/// Assemble a fork without promoting task text into System authority.
pub(super) fn assemble(
    context: &ToolContext,
    identity: &ForkIdentity<'_>,
    request: &str,
    requirements: &[ForkRequirement],
) -> ForkPrompt {
    let mut parent_plan = inherited_parent_plan(context);
    // A fork publishes the plan it inherited, never its own identity block.
    // Removing this source also fails safe for an incorrectly assembled
    // embedder context and guarantees one current fork identity at every depth.
    parent_plan.remove(PromptSource::ForkAgentPolicy);

    let mut active_plan = parent_plan.clone();
    active_plan.set(PromptSource::ForkAgentPolicy, build_fork_preamble(identity));

    ForkPrompt {
        parent_plan,
        active_plan,
        user_task: build_user_task(request, requirements),
    }
}

fn inherited_parent_plan(context: &ToolContext) -> PromptPlan {
    if let Some(parent) = context.get_extension::<ParentPromptPlan>() {
        return parent.plan().clone();
    }

    let mut plan = PromptPlan::new();
    if let Some(legacy) = context.get_extension::<ParentSystemInstruction>() {
        plan.set(PromptSource::EmbedderPolicy, legacy.as_str());
    }
    plan
}

fn build_user_task(request: &str, requirements: &[ForkRequirement]) -> String {
    if requirements.is_empty() {
        return request.to_owned();
    }

    let mut task = request.to_owned();
    task.push_str("\n\n## Requirements\n");
    for requirement in requirements {
        let _ = write!(
            task,
            "\n### {}\n{}\n",
            requirement.name, requirement.description
        );
    }
    task
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
    use crate::system_prompt::PromptAuthority;

    fn identity(policy: &ChildPolicy) -> ForkIdentity<'_> {
        ForkIdentity {
            parent_agent_id: "parent-id",
            path_address: "root/fork-1",
            granted: policy,
        }
    }

    fn policy() -> ChildPolicy {
        ChildPolicy {
            messaging: MessagingScope::ParentOnly,
            delegation: DelegationBudget {
                remaining_depth: 0,
                max_concurrent_children: 0,
            },
            inbound_capacity: 4,
            loop_config: None,
        }
    }

    #[test]
    fn typed_parent_roles_survive_and_task_is_user_only() {
        let context = ToolContext::empty();
        let mut inherited = PromptPlan::new();
        inherited.set(PromptSource::ProductPolicy, "SYSTEM-SENTINEL");
        inherited.set(PromptSource::OperatorProfile, "DEVELOPER-SENTINEL");
        inherited.set(PromptSource::WorkspaceProfile, "USER-SEED-SENTINEL");
        context.insert_extension(Arc::new(ParentPromptPlan::new(inherited)));
        let requirements = vec![ForkRequirement {
            name: "REQUIREMENT-NAME".to_owned(),
            description: "REQUIREMENT-DESCRIPTION".to_owned(),
        }];
        let granted = policy();

        let prompt = assemble(
            &context,
            &identity(&granted),
            "TASK-SENTINEL",
            &requirements,
        );
        let authorities = prompt
            .active_plan
            .fragments()
            .iter()
            .map(crate::system_prompt::PromptFragment::authority)
            .collect::<Vec<_>>();

        assert_eq!(
            authorities,
            [
                PromptAuthority::System,
                PromptAuthority::System,
                PromptAuthority::Developer,
                PromptAuthority::User,
            ]
        );
        assert_eq!(prompt.user_task.matches("TASK-SENTINEL").count(), 1);
        assert_eq!(prompt.user_task.matches("REQUIREMENT-NAME").count(), 1);
        assert_eq!(
            prompt.user_task.matches("REQUIREMENT-DESCRIPTION").count(),
            1
        );
        assert!(prompt.active_plan.fragments().iter().all(|fragment| {
            !fragment.content().contains("TASK-SENTINEL")
                && !fragment.content().contains("REQUIREMENT-NAME")
                && !fragment.content().contains("REQUIREMENT-DESCRIPTION")
        }));
    }

    #[test]
    fn legacy_parent_instruction_is_explicit_embedder_policy() {
        let context = ToolContext::empty();
        context.insert_extension(Arc::new(ParentSystemInstruction::new(
            "LEGACY-EMBEDDER-SENTINEL",
        )));
        let granted = policy();

        let prompt = assemble(&context, &identity(&granted), "task", &[]);
        let inherited = prompt.parent_plan.fragments();

        assert_eq!(inherited.len(), 1);
        assert_eq!(inherited[0].source(), PromptSource::EmbedderPolicy);
        assert_eq!(inherited[0].authority(), PromptAuthority::System);
        assert_eq!(inherited[0].content(), "LEGACY-EMBEDDER-SENTINEL");
    }

    #[test]
    fn typed_parent_plan_wins_over_legacy_bridge_when_both_exist() {
        let context = ToolContext::empty();
        let mut typed = PromptPlan::new();
        typed.set(PromptSource::ProductPolicy, "TYPED-PRODUCT-SENTINEL");
        context.insert_extension(Arc::new(ParentPromptPlan::new(typed)));
        context.insert_extension(Arc::new(ParentSystemInstruction::new(
            "STALE-LEGACY-SENTINEL",
        )));
        let granted = policy();

        let prompt = assemble(&context, &identity(&granted), "task", &[]);

        assert!(
            prompt
                .parent_plan
                .fragments()
                .iter()
                .any(|fragment| fragment.content() == "TYPED-PRODUCT-SENTINEL")
        );
        assert!(
            prompt
                .active_plan
                .fragments()
                .iter()
                .all(|fragment| !fragment.content().contains("STALE-LEGACY-SENTINEL"))
        );
    }

    #[test]
    fn fork_of_fork_replaces_stale_identity_and_publishes_none() {
        let context = ToolContext::empty();
        let mut inherited = PromptPlan::new();
        inherited.set(PromptSource::ProductPolicy, "product");
        inherited.set(PromptSource::ForkAgentPolicy, "STALE-FORK-IDENTITY");
        context.insert_extension(Arc::new(ParentPromptPlan::new(inherited)));
        let granted = policy();

        let prompt = assemble(&context, &identity(&granted), "task", &[]);

        assert!(prompt.parent_plan.fragments().iter().all(|fragment| {
            fragment.source() != PromptSource::ForkAgentPolicy
                && !fragment.content().contains("STALE-FORK-IDENTITY")
        }));
        assert_eq!(
            prompt
                .active_plan
                .fragments()
                .iter()
                .filter(|fragment| fragment.source() == PromptSource::ForkAgentPolicy)
                .count(),
            1
        );
    }

    #[test]
    fn fork_system_identity_preserves_seed_but_inherited_non_system_change_cuts() {
        use crate::system_prompt::PromptSeedFingerprint;

        let context = ToolContext::empty();
        let mut inherited = PromptPlan::new();
        inherited.set(PromptSource::ProductPolicy, "product-v1");
        inherited.set(PromptSource::OperatorProfile, "operator-v1");
        inherited.set(PromptSource::WorkspaceProfile, "workspace-v1");
        context.insert_extension(Arc::new(ParentPromptPlan::new(inherited)));
        let granted = policy();

        let prompt = assemble(&context, &identity(&granted), "task", &[]);
        assert_eq!(
            PromptSeedFingerprint::from_plan(&prompt.parent_plan),
            PromptSeedFingerprint::from_plan(&prompt.active_plan),
            "the fresh System-only fork identity must preserve the inherited anchor seed",
        );

        let mut changed = prompt.parent_plan.clone();
        changed.set(PromptSource::OperatorProfile, "operator-v2");
        assert_ne!(
            PromptSeedFingerprint::from_plan(&prompt.parent_plan),
            PromptSeedFingerprint::from_plan(&changed),
            "a changed inherited Developer seed must cut the anchor",
        );
        changed.set(PromptSource::OperatorProfile, "operator-v1");
        changed.set(PromptSource::WorkspaceProfile, "workspace-v2");
        assert_ne!(
            PromptSeedFingerprint::from_plan(&prompt.parent_plan),
            PromptSeedFingerprint::from_plan(&changed),
            "a changed inherited User seed must cut the anchor",
        );
    }
}
