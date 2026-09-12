//! Name lookup refuses ambiguity and respects explicit directory scope without restricting IDs.

use super::super::super::types::SessionPersistError;
use super::tests::entry;
use super::{name_directory_matches, resolve_in_entries, resolve_in_entries_with_scope};
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn duplicate_names_are_errors_globally_and_within_the_selected_project() -> TestResult {
    let first = entry("12345678-first", Some("shared"), 1);
    let second = entry("87654321-second", Some("shared"), 2);
    for scope in [None, Some(Path::new("/work"))] {
        let result =
            resolve_in_entries_with_scope(vec![first.clone(), second.clone()], "shared", scope);
        let Err(SessionPersistError::AmbiguousName { name, matches }) = result else {
            return Err("duplicate name was not refused".into());
        };
        assert_eq!(name, "shared");
        assert_eq!(matches, vec![first.id.clone(), second.id.clone()]);
    }
    Ok(())
}

#[test]
fn same_name_in_other_projects_does_not_win_but_ids_and_prefixes_remain_global() -> TestResult {
    let first = entry("12345678-first", Some("shared"), 1);
    let mut other = entry("87654321-other", Some("shared"), 2);
    other.working_dir = "/elsewhere".to_owned();
    let entries = vec![other.clone(), first.clone()];
    let scope = Some(Path::new("/work"));
    assert_eq!(
        resolve_in_entries_with_scope(entries.clone(), " shared ", scope)?.id,
        first.id
    );
    assert_eq!(
        resolve_in_entries_with_scope(entries.clone(), "", scope)?.id,
        first.id
    );
    assert_eq!(
        resolve_in_entries_with_scope(entries.clone(), &other.id, scope)?.id,
        other.id
    );
    assert_eq!(
        resolve_in_entries_with_scope(entries.clone(), "87654321", scope)?.id,
        other.id
    );
    assert!(matches!(
        resolve_in_entries_with_scope(entries.clone(), "8765432", scope),
        Err(SessionPersistError::NotFound { .. })
    ));
    assert!(matches!(
        resolve_in_entries(entries.clone(), "shared"),
        Err(SessionPersistError::AmbiguousName { .. })
    ));
    assert!(matches!(
        resolve_in_entries_with_scope(entries, "shared", Some(Path::new("/missing-project"))),
        Err(SessionPersistError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn full_id_precedes_a_conflicting_name_and_prefix_ambiguity_is_preserved() -> TestResult {
    let first = entry("12345678-first", Some("shared"), 1);
    let second = entry("12345678-second", Some(&first.id), 2);
    let entries = vec![second, first.clone()];
    assert_eq!(resolve_in_entries(entries.clone(), &first.id)?.id, first.id);
    assert!(
        matches!(resolve_in_entries(entries, "12345678"), Err(SessionPersistError::AmbiguousPrefix { matches, .. }) if matches.len()==2)
    );
    Ok(())
}

#[test]
fn missing_directories_match_only_the_same_recorded_path() -> TestResult {
    let temp = tempfile::tempdir()?;
    let missing = temp.path().join("missing");
    assert!(name_directory_matches(&missing, &missing)?);
    assert!(!name_directory_matches(
        &missing,
        &temp.path().join("other")
    )?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_project_matches_and_canonicalization_errors_are_not_hidden() -> TestResult {
    let temp = tempfile::tempdir()?;
    let real = temp.path().join("real");
    std::fs::create_dir(&real)?;
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias)?;
    assert!(name_directory_matches(&real, &alias)?);
    let loop_path = temp.path().join("loop");
    std::os::unix::fs::symlink(&loop_path, &loop_path)?;
    assert!(
        matches!(name_directory_matches(&loop_path, &real), Err(SessionPersistError::NameScopeDirectory { path, .. }) if path==loop_path)
    );
    Ok(())
}
