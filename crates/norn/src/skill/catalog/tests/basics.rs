use super::*;

#[test]
fn assert_catalog_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SkillCatalog>();
}

#[test]
fn empty_catalog_reports_is_empty_and_blank_listing() {
    let catalog = SkillCatalog::scan(&[]);
    assert!(catalog.is_empty());
    assert_eq!(catalog.len(), 0);
    assert!(catalog.list().is_empty());
    assert!(catalog.get("anything").is_none());
    assert!(catalog.system_prompt_listing().is_empty());
    assert!(catalog.diagnostics().is_empty());
}

#[test]
fn missing_directory_is_silent_and_yields_empty_catalog() -> TestResult {
    let dir = tempdir()?;
    let missing = dir.path().join("does-not-exist");
    let catalog = SkillCatalog::scan(&[missing]);
    assert!(catalog.is_empty());
    assert!(catalog.diagnostics().is_empty());
    Ok(())
}

#[test]
fn scan_lists_loaded_skills_in_discovery_order() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "alpha", "first")?;
    write_skill(dir.path(), "beta", "second")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    assert_eq!(catalog.len(), 2);
    let listed = catalog.list();
    let names: Vec<&str> = listed.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    Ok(())
}

#[test]
fn get_returns_metadata_for_known_skill_and_none_for_unknown() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "lookup", "find me")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let meta = catalog.get("lookup").ok_or("metadata missing")?;
    assert_eq!(meta.description.as_deref(), Some("find me"));
    assert!(catalog.get("not-here").is_none());
    Ok(())
}

#[test]
fn first_directory_wins_on_name_collision() -> TestResult {
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    write_skill(dir_a.path(), "shared", "from A")?;
    write_skill(dir_b.path(), "shared", "from B")?;

    let catalog = SkillCatalog::scan(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);

    assert_eq!(catalog.len(), 1);
    let list = catalog.list();
    assert_eq!(list.len(), 1);
    let meta = catalog.get("shared").ok_or("metadata missing")?;
    assert_eq!(meta.description.as_deref(), Some("from A"));
    Ok(())
}

#[test]
fn shadow_diagnostic_records_the_shadowed_path() -> TestResult {
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    write_skill(dir_a.path(), "deploy", "from A")?;
    write_skill(dir_b.path(), "deploy", "from B")?;

    let catalog = SkillCatalog::scan(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);

    let shadow_path = dir_b.path().join("deploy").join("SKILL.md");
    let winning_path = dir_a.path().join("deploy").join("SKILL.md");
    let shadow_str = shadow_path.display().to_string();
    let winning_str = winning_path.display().to_string();

    let shadow_diag = catalog
        .diagnostics()
        .iter()
        .find(|d| d.code == "skill-shadowed")
        .ok_or("shadow diagnostic was not emitted")?;

    assert_eq!(shadow_diag.severity, DiagnosticSeverity::Warning);
    assert_eq!(shadow_diag.file_path.as_deref(), Some(shadow_str.as_str()));
    assert!(
        shadow_diag
            .suggestion
            .as_deref()
            .is_some_and(|s| s.contains(&winning_str)),
        "suggestion should reference winning path, got {:?}",
        shadow_diag.suggestion
    );
    assert_eq!(shadow_diag.source_tool.as_deref(), Some(SOURCE_TOOL));
    Ok(())
}

#[test]
fn loader_skip_diagnostics_propagate_into_catalog() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("broken").join("SKILL.md"),
        "---\nname: broken\n---\nbody\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    assert!(catalog.is_empty());
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|d| d.code == "skill-missing-description"),
        "missing-description diagnostic should reach catalog, got {:?}",
        catalog.diagnostics()
    );
    Ok(())
}

#[test]
fn per_skill_warning_diagnostics_propagate_into_catalog() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("real-dir").join("SKILL.md"),
        "---\nname: different-name\ndescription: hi\n---\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    assert_eq!(catalog.len(), 1);
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|d| d.code == "skill-name-mismatch"),
        "name-mismatch diagnostic should reach catalog, got {:?}",
        catalog.diagnostics()
    );
    Ok(())
}
