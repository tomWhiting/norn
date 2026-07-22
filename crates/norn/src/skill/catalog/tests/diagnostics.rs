use super::*;

#[test]
fn allowed_tools_emits_info_diagnostic() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("greet").join("SKILL.md"),
        "---\ndescription: hi\nallowed-tools: Read Write\n---\nbody\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let diag = catalog
        .diagnostics()
        .iter()
        .find(|d| d.code == "skill-allowed-tools-not-enforced")
        .ok_or("allowed-tools-not-enforced diagnostic was not emitted")?;
    assert_eq!(diag.severity, DiagnosticSeverity::Info);
    assert_eq!(diag.source_tool.as_deref(), Some(SOURCE_TOOL));
    assert!(diag.message.contains("greet"));
    assert!(diag.suggestion.is_none());
    Ok(())
}

#[test]
fn no_allowed_tools_diagnostic_when_field_absent() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "plain", "no allowed tools")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    assert!(
        !catalog
            .diagnostics()
            .iter()
            .any(|d| d.code == "skill-allowed-tools-not-enforced"),
        "should not emit when allowed-tools is absent",
    );
    Ok(())
}

#[test]
fn no_allowed_tools_diagnostic_when_field_empty() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("empty-tools").join("SKILL.md"),
        "---\ndescription: hi\nallowed-tools: \"\"\n---\nbody\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    assert!(
        !catalog
            .diagnostics()
            .iter()
            .any(|d| d.code == "skill-allowed-tools-not-enforced"),
        "should not emit for empty allowed-tools",
    );
    Ok(())
}

#[test]
fn shadowed_skill_does_not_emit_allowed_tools_diagnostic() -> TestResult {
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    write_skill(dir_a.path(), "deploy", "from A")?;
    write_file(
        &dir_b.path().join("deploy").join("SKILL.md"),
        "---\ndescription: from B\nallowed-tools: Read\n---\nbody\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);
    // The B copy is shadowed; only A wins and A has no allowed-tools.
    assert!(
        !catalog
            .diagnostics()
            .iter()
            .any(|d| d.code == "skill-allowed-tools-not-enforced"),
        "shadowed skill should not produce allowed-tools diagnostic",
    );
    Ok(())
}
