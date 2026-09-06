//! Opaque preference transaction regressions against both real settings writers.

use super::*;
use serde_json::json;
use std::io;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn map(value: Value) -> Result<Map<String, Value>, Box<dyn std::error::Error>> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err("test replacement must be an object".into()),
    }
}

#[test]
fn capture_performs_no_file_creation_and_keeps_whole_layer_precedence() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let snapshot =
        TuiPreferencesSnapshot::from_layer(TuiPreferenceScope::WorkspaceLocal, &root, None)?;
    assert_eq!(snapshot.path(), root.join(".norn/settings.local.json"));
    assert!(!root.join(".norn").exists());
    let layers = super::super::TuiPreferencesLayers {
        user: Some(json!({"view":{"changes":true}})),
        project: Some(json!({"composer":{}})),
        local: None,
    };
    assert_eq!(
        layers.winning_layer(),
        Some(super::super::TuiPreferenceLayer::SharedProject)
    );
    assert_eq!(
        layers.value(super::super::TuiPreferenceLayer::SharedProject),
        Some(&json!({"composer":{}}))
    );
    assert!(
        TuiPreferencesSnapshot::from_layer(
            TuiPreferenceScope::WorkspaceLocal,
            &root,
            Some(json!([]))
        )
        .is_err()
    );
    assert!(!root.join(".norn").exists());
    Ok(())
}

#[test]
fn workspace_save_preserves_concurrent_mcp_and_unowned_frontend_values() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    std::fs::create_dir(root.join(".norn"))?;
    let initial = json!({"view":{"changes":false},"composer":{"send_key":"enter"}});
    let snapshot = TuiPreferencesSnapshot::from_layer(
        TuiPreferenceScope::WorkspaceLocal,
        &root,
        Some(initial.clone()),
    )?;
    std::fs::write(
        snapshot.path(),
        serde_json::to_vec(&json!({"tui":initial,"future":{"kept":true}}))?,
    )?;
    let mutation = super::super::McpPersistentMutation::Upsert {
        name: "parallel".to_owned(),
        definition: super::super::McpServerSettings {
            command: Some("test-server".to_owned()),
            ..super::super::McpServerSettings::default()
        },
    };
    super::super::mcp_patch::persist_mcp_mutation(
        &root,
        super::super::McpPersistentScope::WorkspaceLocal,
        &mutation,
    )?;
    let result = snapshot.patch(
        &["view", "display", "input"],
        &map(json!({"view":{"changes":true}}))?,
    )?;
    assert!(matches!(
        result.publication,
        SettingsPublication::PublishedDurable
    ));
    let stored: Value = serde_json::from_slice(&std::fs::read(snapshot.path())?)?;
    assert_eq!(stored["mcp_servers"]["parallel"]["command"], "test-server");
    assert_eq!(stored["tui"]["composer"]["send_key"], "enter");
    assert_eq!(stored["future"]["kept"], true);
    assert_eq!(stored["tui"]["view"]["changes"], true);
    assert!(matches!(
        result
            .snapshot
            .patch(&["view"], &map(json!({"view":{"changes":true}}))?)?
            .publication,
        SettingsPublication::Unchanged
    ));
    let conflict = snapshot.patch(&["view"], &map(json!({"view":{}}))?);
    assert!(matches!(conflict, Err(TuiPreferencesError::Conflict { key, .. }) if key == "view"));
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(snapshot.path())?)?,
        stored
    );
    Ok(())
}

#[test]
fn private_save_preserves_new_unowned_keys_and_reset_removes_only_owned_keys() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let path = root.join("private/settings.json");
    let snapshot = TuiPreferencesSnapshot {
        scope: TuiPreferenceScope::User,
        project_root: root.clone(),
        path: path.clone(),
        original: None,
    };
    let first = snapshot.patch(&["view"], &map(json!({"view":{"changes":true}}))?)?;
    assert!(matches!(
        first.publication,
        SettingsPublication::PublishedDurable
    ));
    assert!(root.join("private/.mcp-settings.lock").is_file());
    let external = json!({"tui":{"view":{"changes":true},"extension":{"keep":1}},"mcp_servers":{}});
    std::fs::write(&path, serde_json::to_vec(&external)?)?;
    let reset = first.snapshot.patch(&["view"], &Map::new())?;
    assert!(matches!(
        reset.publication,
        SettingsPublication::PublishedDurable
    ));
    let stored: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(
        stored,
        json!({"tui":{"extension":{"keep":1}},"mcp_servers":{}})
    );
    assert_eq!(
        reset.snapshot.original(),
        Some(&json!({"extension":{"keep":1}}))
    );
    Ok(())
}

#[test]
fn missing_and_null_owned_values_conflict_and_unowned_patch_is_refused() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let snapshot =
        TuiPreferencesSnapshot::from_layer(TuiPreferenceScope::WorkspaceLocal, &root, None)?;
    assert!(matches!(
        snapshot.patch(&["view"], &map(json!({"composer":{}}))?),
        Err(TuiPreferencesError::InvalidPatch { .. })
    ));
    assert!(!root.join(".norn").exists());
    std::fs::create_dir(root.join(".norn"))?;
    std::fs::write(snapshot.path(), b"{\"tui\":{\"view\":null}}")?;
    assert!(matches!(
        snapshot.patch(&["view"], &Map::new()),
        Err(TuiPreferencesError::Conflict { .. })
    ));
    assert_eq!(
        std::fs::read(snapshot.path())?,
        b"{\"tui\":{\"view\":null}}"
    );
    Ok(())
}

#[test]
fn empty_owned_reset_removes_whole_object_shadow_without_erasing_document() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let snapshot = TuiPreferencesSnapshot::from_layer(
        TuiPreferenceScope::WorkspaceLocal,
        &root,
        Some(json!({})),
    )?;
    std::fs::create_dir(root.join(".norn"))?;
    std::fs::write(snapshot.path(), b"{\"tui\":{},\"model\":\"kept\"}")?;
    let result = snapshot.patch(&["view", "display", "input"], &Map::new())?;
    assert!(result.snapshot.original().is_none());
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(snapshot.path())?)?,
        json!({"model":"kept"})
    );
    Ok(())
}

#[test]
fn directory_sync_failure_is_a_published_result_with_its_original_cause() -> TestResult {
    let path = Path::new("/fixture/settings.json");
    let outcome =
        SettingsPublication::after_directory_sync(path, Err(io::Error::from_raw_os_error(5)));
    let SettingsPublication::PublishedDurabilityUncertain(error) = outcome else {
        return Err("post-publication error was not retained".into());
    };
    assert!(error.published());
    assert_eq!(error.path(), path);
    assert_eq!(error.io_error().raw_os_error(), Some(5));
    assert!(error.to_string().contains("already published: true"));
    assert!(matches!(
        SettingsPublication::after_directory_sync(path, Ok(())),
        SettingsPublication::PublishedDurable
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn failed_symlink_target_is_not_published_and_outside_file_survives() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let snapshot =
        TuiPreferencesSnapshot::from_layer(TuiPreferenceScope::WorkspaceLocal, &root, None)?;
    std::fs::create_dir(root.join(".norn"))?;
    let outside = root.join("outside.json");
    std::fs::write(&outside, b"outside")?;
    std::os::unix::fs::symlink(&outside, snapshot.path())?;
    let result = snapshot.patch(&["view"], &map(json!({"view":{}}))?);
    assert!(matches!(result, Err(TuiPreferencesError::Filesystem(error)) if !error.published()));
    assert_eq!(std::fs::read(outside)?, b"outside");
    Ok(())
}
