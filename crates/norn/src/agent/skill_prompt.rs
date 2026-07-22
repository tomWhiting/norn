//! Source-aware skill catalog installation shared by root and child agents.

use crate::r#loop::loop_context::LoopContext;
use crate::skill::{SkillCatalog, SkillPromptSections};
use crate::system_prompt::{PromptPlan, PromptSource};
use crate::tool::registry::ToolRegistry;

/// Whether the child's resolved tool surface can activate skills.
pub(crate) fn child_skill_tool_available(
    parent_registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> bool {
    parent_registry.get("skill").is_some()
        && allow_list.is_none_or(|list| list.iter().any(|name| name == "skill"))
}

/// Install root skill fragments while retaining their trust provenance.
///
/// `base_suffix` remains a flattened compatibility view. Provider assembly
/// consumes the typed plan populated here.
pub(crate) fn apply_skill_listing(
    loop_context: &mut LoopContext,
    catalog: &SkillCatalog,
    skill_tool_available: bool,
) {
    let rebuild_installed_plan = loop_context.stable_prompt_plan.is_some();
    let sections = if skill_tool_available {
        catalog.prompt_sections()
    } else {
        SkillPromptSections::default()
    };
    loop_context.base_suffix = sections.flattened_content();
    if loop_context.stable_prompt_plan.is_some() || !sections.is_empty() {
        let plan = loop_context
            .stable_prompt_plan
            .get_or_insert_with(PromptPlan::new);
        set_skill_prompt_sections(plan, &sections);
    }
    if rebuild_installed_plan {
        loop_context.rebuild_base_section();
    }
}

/// Give a child the same source-aware skill fragments as the root.
pub(crate) fn install_child_skill_listing(
    loop_context: &mut LoopContext,
    catalog: &SkillCatalog,
    skill_tool_available: bool,
) {
    let sections = catalog.prompt_sections();
    let typed_listing = sections.flattened_content();
    let legacy_listing = catalog.system_prompt_listing();
    if !skill_tool_available || typed_listing.is_empty() {
        remove_child_skill_listing(loop_context, &typed_listing, &legacy_listing);
        return;
    }

    let mut plan = loop_context.stable_prompt_plan.take().unwrap_or_else(|| {
        let mut plan = PromptPlan::new();
        plan.set(
            PromptSource::ChildAgentPolicy,
            strip_known_listing_suffix(
                &loop_context.base_system_instruction(),
                &typed_listing,
                &legacy_listing,
            ),
        );
        plan
    });
    set_skill_prompt_sections(&mut plan, &sections);
    loop_context.base_suffix = typed_listing;
    loop_context.install_stable_prompt_plan(plan);
}

fn remove_child_skill_listing(
    loop_context: &mut LoopContext,
    typed_listing: &str,
    legacy_listing: &str,
) {
    loop_context.base_suffix.clear();
    if let Some(mut plan) = loop_context.stable_prompt_plan.take() {
        set_skill_prompt_sections(&mut plan, &SkillPromptSections::default());
        loop_context.install_stable_prompt_plan(plan);
        return;
    }

    let cleaned = strip_known_listing_suffix(
        &loop_context.base_system_instruction(),
        typed_listing,
        legacy_listing,
    );
    if let Some(base) = loop_context.system_sections.first_mut() {
        base.clone_from(&cleaned);
    }
    loop_context.base_prefix = cleaned;
}

fn set_skill_prompt_sections(plan: &mut PromptPlan, sections: &SkillPromptSections) {
    plan.set(PromptSource::SkillCatalogPolicy, sections.policy());
    plan.set(
        PromptSource::OperatorSkillCatalog,
        sections.operator_entries(),
    );
    plan.set(
        PromptSource::WorkspaceSkillCatalog,
        sections.workspace_entries(),
    );
}

fn strip_known_listing_suffix(base: &str, typed_listing: &str, legacy_listing: &str) -> String {
    strip_exact_suffix(base, typed_listing)
        .or_else(|| strip_exact_suffix(base, legacy_listing))
        .unwrap_or(base)
        .to_owned()
}

fn strip_exact_suffix<'a>(base: &'a str, listing: &str) -> Option<&'a str> {
    if listing.is_empty() {
        return None;
    }
    if base == listing {
        return Some("");
    }
    base.strip_suffix(listing)
        .and_then(|prefix| prefix.strip_suffix("\n\n"))
}
