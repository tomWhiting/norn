use super::*;

#[test]
fn system_prompt_listing_starts_with_header_and_instruction() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "deploy", "Deploy the service.")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let listing = catalog.system_prompt_listing();
    assert!(
        listing.starts_with("# Available Skills\n\n"),
        "listing should start with the heading, got: {listing}"
    );
    assert!(
        listing
            .contains("The following skills provide specialized instructions for specific tasks."),
        "listing should include the behavioral instruction line, got: {listing}"
    );
    assert!(
        listing.contains("call the skill tool with that"),
        "listing should include call-the-tool instruction, got: {listing}"
    );
    Ok(())
}

#[test]
fn prompt_sections_partition_operator_and_workspace_metadata() -> TestResult {
    let root = tempdir()?;
    let operator_dir = root.path().join("operator");
    let workspace = root.path().join("workspace");
    let workspace_dir = workspace.join(".norn/skills");
    write_skill(&operator_dir, "trusted", "OPERATOR_DESCRIPTION_SENTINEL")?;
    write_file(
        &workspace_dir.join("repository/SKILL.md"),
        "---\ndescription: WORKSPACE_DESCRIPTION_SENTINEL\n\
         when-to-use: WORKSPACE_WHEN_SENTINEL\n---\nbody\n",
    )?;
    let workspace_root = workspace.canonicalize()?;

    let catalog =
        SkillCatalog::scan_with_workspace(&[workspace_dir, operator_dir], &workspace_root);
    let sections = catalog.prompt_sections();

    assert_eq!(catalog.origin("trusted"), Some(SkillOrigin::Operator));
    assert_eq!(catalog.origin("repository"), Some(SkillOrigin::Workspace));
    assert!(sections.policy().contains("# Available Skills"));
    assert!(
        sections
            .operator_entries()
            .contains("OPERATOR_DESCRIPTION_SENTINEL")
    );
    assert!(
        !sections
            .operator_entries()
            .contains("WORKSPACE_DESCRIPTION_SENTINEL")
    );
    assert!(
        sections
            .workspace_entries()
            .contains("WORKSPACE_DESCRIPTION_SENTINEL")
    );
    assert!(
        sections
            .workspace_entries()
            .contains("WORKSPACE_WHEN_SENTINEL")
    );
    assert!(
        !sections
            .workspace_entries()
            .contains("OPERATOR_DESCRIPTION_SENTINEL")
    );
    assert!(!sections.policy().contains("OPERATOR_DESCRIPTION_SENTINEL"));
    assert!(!sections.policy().contains("WORKSPACE_DESCRIPTION_SENTINEL"));
    Ok(())
}

#[test]
fn system_prompt_listing_concatenates_when_to_use() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("fix-issue").join("SKILL.md"),
        "---\ndescription: Fix the issue.\nwhen-to-use: Use when a bug is reported.\n---\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let listing = catalog.system_prompt_listing();
    assert!(
        listing.contains("- fix-issue: Fix the issue. Use when a bug is reported."),
        "expected description + when_to_use concatenation, got: {listing}"
    );
    Ok(())
}

#[test]
fn system_prompt_listing_omits_when_to_use_when_absent() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "simple", "Just a description")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let listing = catalog.system_prompt_listing();
    assert!(
        listing.contains("- simple: Just a description"),
        "expected bullet for simple skill, got: {listing}"
    );
    assert!(
        !listing.contains("- simple: Just a description "),
        "no trailing space when when_to_use is absent, got: {listing:?}"
    );
    Ok(())
}

#[test]
fn system_prompt_listing_excludes_disable_model_invocation_skills() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("hidden").join("SKILL.md"),
        "---\ndescription: hidden one\ndisable-model-invocation: true\n---\n",
    )?;
    write_skill(dir.path(), "visible", "visible one")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    // list() still surfaces both for workflow authors.
    assert_eq!(catalog.list().len(), 2);

    let listing = catalog.system_prompt_listing();
    assert!(
        !listing.contains("hidden"),
        "hidden skill must not appear in listing, got: {listing}"
    );
    assert!(
        listing.contains("- visible: visible one"),
        "visible skill must appear in listing, got: {listing}"
    );
    Ok(())
}

#[test]
fn system_prompt_listing_is_empty_when_all_skills_are_hidden() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("hidden-a").join("SKILL.md"),
        "---\ndescription: a\ndisable-model-invocation: true\n---\n",
    )?;
    write_file(
        &dir.path().join("hidden-b").join("SKILL.md"),
        "---\ndescription: b\ndisable-model-invocation: true\n---\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    assert_eq!(catalog.len(), 2);
    assert!(
        catalog.system_prompt_listing().is_empty(),
        "listing should be empty when no skills are model-invocable"
    );
    Ok(())
}
