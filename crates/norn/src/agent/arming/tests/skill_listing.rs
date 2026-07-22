use super::super::*;
use crate::skill::SkillCatalog;
use crate::system_prompt::PromptSource;

/// A child's skill listing is gated on the `skill` tool being on the
/// child's resolved surface: present + admitted → available; present +
/// excluded by allow-list → unavailable; absent registry → unavailable.
#[test]
fn child_skill_tool_available_respects_registry_and_allow_list() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(crate::tools::skill::SkillTool::new()));

    assert!(child_skill_tool_available(&registry, None));
    assert!(child_skill_tool_available(
        &registry,
        Some(&["skill".to_owned(), "read".to_owned()]),
    ));
    assert!(!child_skill_tool_available(
        &registry,
        Some(&["read".to_owned()])
    ));

    let empty = ToolRegistry::new();
    assert!(!child_skill_tool_available(&empty, None));
}

/// The shared child-listing installer folds the "# Available Skills"
/// section into the child's base instruction (after the base) when the
/// skill tool is available, and leaves the instruction untouched when it
/// is not — the same filtered listing the root gets.
#[test]
fn install_child_skill_listing_appends_when_available_and_skips_when_not()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let skill_dir = dir.path().join("greet");
    std::fs::create_dir(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: greet the user\n---\nbody",
    )?;
    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);

    let mut available = LoopContext::new("You are a sub-agent.");
    install_child_skill_listing(&mut available, &catalog, true);
    let base = available.base_system_instruction();
    assert!(
        base.contains("You are a sub-agent."),
        "base retained: {base}"
    );
    assert!(
        base.contains("# Available Skills"),
        "listing present when available: {base}",
    );
    assert!(
        base.find("You are a sub-agent.") < base.find("# Available Skills"),
        "the base instruction must precede the listing: {base}",
    );

    install_child_skill_listing(&mut available, &catalog, false);
    assert_eq!(available.base_system_instruction(), "You are a sub-agent.");
    assert!(available.base_suffix.is_empty());
    let remaining_sources = available
        .stable_prompt_plan()
        .ok_or("typed child plan was not installed")?
        .fragments()
        .iter()
        .map(crate::system_prompt::PromptFragment::source)
        .collect::<Vec<_>>();
    assert!(
        [
            PromptSource::SkillCatalogPolicy,
            PromptSource::OperatorSkillCatalog,
            PromptSource::WorkspaceSkillCatalog,
        ]
        .iter()
        .all(|source| !remaining_sources.contains(source)),
        "gating the skill tool must remove every inherited catalog source",
    );

    let mut gated = LoopContext::new("You are a sub-agent.");
    install_child_skill_listing(&mut gated, &catalog, false);
    assert_eq!(
        gated.base_system_instruction(),
        "You are a sub-agent.",
        "an unavailable skill tool leaves the child's instruction untouched",
    );
    Ok(())
}

/// Regression: an embedder-supplied parent base
/// (`ParentSystemInstruction`) may already contain the listing — the
/// root's `base_system_instruction()` includes its materialized
/// `base_suffix`. Installing on such a base must not duplicate the
/// "# Available Skills" section.
#[test]
fn install_child_skill_listing_does_not_duplicate_listing_bearing_base()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path().join("workspace");
    let skill_dir = workspace.join(".norn/skills/greet");
    let operator_dir = dir.path().join("operator");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::create_dir_all(operator_dir.join("trusted"))?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: WORKSPACE_CHILD_SKILL_SENTINEL\n---\nbody",
    )?;
    std::fs::write(
        operator_dir.join("trusted/SKILL.md"),
        "---\ndescription: OPERATOR_CHILD_SKILL_SENTINEL\n---\nbody",
    )?;
    let workspace_root = workspace.canonicalize()?;
    let catalog = SkillCatalog::scan_with_workspace(
        &[workspace.join(".norn/skills"), operator_dir],
        &workspace_root,
    );

    // A parent base that already carries the exact generated listing,
    // as a root's materialized instruction would.
    let listing_bearing_base = format!(
        "You are the parent.\n\n{}",
        catalog.prompt_sections().flattened_content()
    );
    let mut child = LoopContext::new(&listing_bearing_base);
    install_child_skill_listing(&mut child, &catalog, true);

    let base = child.base_system_instruction();
    assert_eq!(
        base.matches("# Available Skills").count(),
        1,
        "the listing must appear exactly once: {base}",
    );
    assert_eq!(
        base, listing_bearing_base,
        "the flattened compatibility view stays byte-identical",
    );
    let plan = child
        .stable_prompt_plan()
        .ok_or("child listing installation did not publish a typed plan")?;
    let workspace_fragment = plan
        .fragments()
        .iter()
        .find(|fragment| fragment.source() == PromptSource::WorkspaceSkillCatalog)
        .ok_or("workspace skill fragment was not published")?;
    assert_eq!(
        workspace_fragment.authority(),
        crate::system_prompt::PromptAuthority::User
    );
    assert!(
        plan.fragments()
            .iter()
            .filter(|fragment| {
                fragment.authority() != crate::system_prompt::PromptAuthority::User
            })
            .all(|fragment| !fragment
                .content()
                .contains("WORKSPACE_CHILD_SKILL_SENTINEL")),
        "repository metadata must not survive in a higher-authority child fragment",
    );
    assert!(
        plan.fragments()
            .iter()
            .find(|fragment| fragment.source() == PromptSource::OperatorSkillCatalog)
            .is_some_and(|fragment| {
                fragment.content().contains("OPERATOR_CHILD_SKILL_SENTINEL")
                    && fragment.authority() == crate::system_prompt::PromptAuthority::Developer
            })
    );

    // Re-installation updates the typed sources without duplicating text.
    install_child_skill_listing(&mut child, &catalog, true);
    assert_eq!(
        child
            .base_system_instruction()
            .matches("# Available Skills")
            .count(),
        1,
        "repeat installs must not duplicate the section",
    );

    let legacy_listing_bearing_base =
        format!("You are the parent.\n\n{}", catalog.system_prompt_listing());
    let mut legacy_gated = LoopContext::new(&legacy_listing_bearing_base);
    install_child_skill_listing(&mut legacy_gated, &catalog, false);
    assert_eq!(
        legacy_gated.base_system_instruction(),
        "You are the parent."
    );
    assert!(legacy_gated.stable_prompt_plan().is_none());
    Ok(())
}
