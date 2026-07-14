use super::*;

fn upsert(name: &str, command: &str) -> McpPersistentMutation {
    McpPersistentMutation::Upsert {
        name: name.to_owned(),
        definition: McpServerSettings {
            command: Some(command.to_owned()),
            ..McpServerSettings::default()
        },
    }
}

#[test]
fn enabled_patch_preserves_unknown_and_unrelated_json() -> Result<(), ConfigError> {
    let original = r#"{
        "future_top_level": {"kept": true},
        "mcp_servers": {
            "docs": {"command": "server", "future_server_key": [1, 2]},
            "other": {"url": "https://example.com/mcp", "future": "kept"}
        }
    }"#;
    let mutation = McpPersistentMutation::SetEnabled {
        name: "docs".to_owned(),
        enabled: false,
    };

    let (bytes, changed) = patch_document(Some(original), &mutation, Path::new("settings.json"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| ConfigError::InvalidConfig {
            reason: error.to_string(),
        })?;

    assert!(changed);
    assert_eq!(value["future_top_level"]["kept"], true);
    assert_eq!(value["mcp_servers"]["docs"]["future_server_key"][1], 2);
    assert_eq!(value["mcp_servers"]["docs"]["enabled"], false);
    assert_eq!(value["mcp_servers"]["other"]["future"], "kept");
    Ok(())
}

#[test]
fn upsert_replaces_only_the_named_whole_entry() -> Result<(), ConfigError> {
    let original = r#"{
        "unknown": 42,
        "mcp_servers": {
            "docs": {"command": "old", "unknown_old": true},
            "other": {"command": "other", "unknown_other": true}
        }
    }"#;

    let (bytes, changed) = patch_document(
        Some(original),
        &upsert("docs", "new"),
        Path::new("settings.json"),
    )?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| ConfigError::InvalidConfig {
            reason: error.to_string(),
        })?;

    assert!(changed);
    assert_eq!(value["unknown"], 42);
    assert_eq!(value["mcp_servers"]["docs"]["command"], "new");
    assert!(value["mcp_servers"]["docs"].get("unknown_old").is_none());
    assert_eq!(value["mcp_servers"]["other"]["unknown_other"], true);
    Ok(())
}

#[test]
fn no_op_remove_does_not_rewrite_or_create_mcp_map() -> Result<(), ConfigError> {
    let mutation = McpPersistentMutation::Remove {
        name: "missing".to_owned(),
    };
    let (bytes, changed) = patch_document(
        Some(r#"{"unknown":true}"#),
        &mutation,
        Path::new("settings.json"),
    )?;

    assert!(!changed);
    assert!(bytes.is_empty());
    Ok(())
}

#[test]
fn malformed_non_object_mcp_map_is_rejected() {
    let error = patch_document(
        Some(r#"{"mcp_servers":[]}"#),
        &upsert("docs", "server"),
        Path::new("settings.json"),
    );
    assert!(error.is_err());
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn private_persistence_rejects_symlink_target() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let home = tempfile::tempdir()?;
    let outside = home.path().join("outside.json");
    std::fs::write(&outside, "outside")?;
    symlink(&outside, home.path().join("settings.json"))?;

    temp_env::with_var("NORN_HOME", Some(home.path()), || {
        let project = tempfile::tempdir()?;
        let result = persist_mcp_mutation(
            &project.path().canonicalize()?,
            McpPersistentScope::User,
            &upsert("docs", "server"),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&outside)?, "outside");
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

#[test]
#[serial_test::serial]
fn project_patch_preserves_unknown_keys_and_never_changes_approval_store()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let canonical = project.path().canonicalize()?;
    std::fs::create_dir(canonical.join(".norn"))?;
    std::fs::write(
        canonical.join(".norn/settings.json"),
        r#"{"unknown":{"kept":true}}"#,
    )?;

    temp_env::with_var("NORN_HOME", Some(home.path()), || {
        let change = persist_mcp_mutation(
            &canonical,
            McpPersistentScope::SharedProject,
            &upsert("docs", "server"),
        )?;
        assert!(change.changed());
        assert!(change.requires_project_approval());
        assert!(!home.path().join("mcp/project-approvals.jsonl").exists());
        let value: Value = serde_json::from_str(&std::fs::read_to_string(change.path())?)?;
        assert_eq!(value["unknown"]["kept"], true);
        assert_eq!(value["mcp_servers"]["docs"]["command"], "server");
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

#[test]
#[serial_test::serial]
fn persistent_scopes_write_only_their_selected_documents() -> Result<(), Box<dyn std::error::Error>>
{
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let canonical = project.path().canonicalize()?;

    temp_env::with_var("NORN_HOME", Some(home.path()), || {
        let user = persist_mcp_mutation(
            &canonical,
            McpPersistentScope::User,
            &upsert("user_server", "user"),
        )?;
        let shared = persist_mcp_mutation(
            &canonical,
            McpPersistentScope::SharedProject,
            &upsert("shared_server", "shared"),
        )?;
        let workspace_local = persist_mcp_mutation(
            &canonical,
            McpPersistentScope::WorkspaceLocal,
            &upsert("workspace_server", "workspace"),
        )?;
        let private_local = persist_mcp_mutation(
            &canonical,
            McpPersistentScope::PrivateLocal,
            &upsert("private_server", "private"),
        )?;

        assert_eq!(user.path(), home.path().join("settings.json"));
        assert_eq!(shared.path(), canonical.join(".norn/settings.json"));
        assert_eq!(
            workspace_local.path(),
            canonical.join(".norn/settings.local.json"),
        );
        assert_eq!(
            private_local.path(),
            super::super::mcp_local::project_local_mcp_settings_path(&canonical)?,
        );
        assert!(!user.requires_project_approval());
        assert!(shared.requires_project_approval());
        assert!(workspace_local.requires_project_approval());
        assert!(!private_local.requires_project_approval());
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}
