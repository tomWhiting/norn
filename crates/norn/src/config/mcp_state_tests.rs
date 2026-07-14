use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;

fn stdio(command: &str) -> McpServerSettings {
    McpServerSettings {
        command: Some(command.to_owned()),
        ..McpServerSettings::default()
    }
}

fn one(command: &str) -> BTreeMap<String, McpServerSettings> {
    BTreeMap::from([("docs".to_owned(), stdio(command))])
}

fn state() -> Result<McpConfigState, ConfigError> {
    McpConfigState::from_layers(
        PathBuf::from("/project"),
        [
            one("user"),
            one("project"),
            one("workspace"),
            one("private"),
        ],
        one("cli"),
    )
}

#[test]
fn exact_precedence_and_full_shadow_chain_are_preserved() -> Result<(), ConfigError> {
    let mut state = state()?;
    state.session_add("docs".to_owned(), stdio("session"))?;

    let inspection = state.inspect("docs")?;
    let effective = inspection
        .effective()
        .ok_or_else(|| missing_server("docs", "inspect"))?;

    assert_eq!(effective.source(), McpConfigLayer::Session);
    assert_eq!(effective.definition().command.as_deref(), Some("session"));
    assert_eq!(inspection.chain().len(), 6);
    assert_eq!(
        inspection
            .chain()
            .iter()
            .map(McpLayerEntry::layer)
            .collect::<Vec<_>>(),
        McpConfigLayer::PRECEDENCE,
    );
    Ok(())
}

#[test]
fn session_remove_reveals_the_next_lower_winner() -> Result<(), ConfigError> {
    let mut state = state()?;
    state.session_add("docs".to_owned(), stdio("session"))?;

    assert!(state.session_remove("docs")?);
    assert!(!state.session_remove("docs")?);

    let snapshot = state.snapshot()?;
    let effective = snapshot
        .get("docs")
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert_eq!(effective.source(), McpConfigLayer::Cli);
    assert_eq!(effective.definition().command.as_deref(), Some("cli"));
    Ok(())
}

#[test]
fn inherited_disable_tracks_reloaded_lower_definition() -> Result<(), ConfigError> {
    let mut state = McpConfigState::from_layers(
        PathBuf::from("/project"),
        [one("user"), BTreeMap::new(), BTreeMap::new(), one("first")],
        BTreeMap::new(),
    )?;

    assert!(state.session_disable("docs")?);
    assert_eq!(
        state.session_entries().get("docs"),
        Some(&McpSessionEntry::DisabledInherited),
    );
    state.private_local = one("reloaded");

    let disabled = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert!(!disabled.enabled());
    assert_eq!(disabled.definition().command.as_deref(), Some("reloaded"));

    assert!(state.session_enable("docs")?);
    assert!(state.session_entries().get("docs").is_none());
    let enabled = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert_eq!(enabled.source(), McpConfigLayer::PrivateLocal);
    assert_eq!(enabled.definition().command.as_deref(), Some("reloaded"));
    Ok(())
}

#[test]
fn session_owned_disable_restores_exact_session_definition() -> Result<(), ConfigError> {
    let mut state = state()?;
    state.session_add("docs".to_owned(), stdio("session"))?;
    state.session_disable("docs")?;
    state.cli = one("changed-cli");

    let disabled = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert!(!disabled.enabled());
    assert_eq!(disabled.definition().command.as_deref(), Some("session"));

    state.session_enable("docs")?;
    let restored = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert!(restored.enabled());
    assert_eq!(restored.definition().command.as_deref(), Some("session"));
    Ok(())
}

#[test]
fn same_name_entries_are_never_field_merged() -> Result<(), ConfigError> {
    let mut user = stdio("user-command");
    user.args = Some(vec!["user-argument".to_owned()]);
    let cli = McpServerSettings {
        url: Some("https://example.com/mcp".to_owned()),
        ..McpServerSettings::default()
    };
    let state = McpConfigState::from_layers(
        PathBuf::from("/project"),
        [
            BTreeMap::from([("docs".to_owned(), user)]),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        ],
        BTreeMap::from([("docs".to_owned(), cli)]),
    )?;

    let effective = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert_eq!(
        effective.definition().url.as_deref(),
        Some("https://example.com/mcp")
    );
    assert!(effective.definition().command.is_none());
    assert!(effective.definition().args.is_none());
    Ok(())
}

#[test]
#[serial_test::serial]
fn disk_reload_retains_cli_and_session_layers() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let norn = project.path().join(".norn");
    std::fs::create_dir(&norn)?;
    std::fs::write(
        norn.join("settings.json"),
        r#"{"mcp_servers":{"docs":{"command":"project-first"}}}"#,
    )?;

    temp_env::with_var("NORN_HOME", Some(home.path()), || {
        let mut state = McpConfigState::load(project.path(), one("cli"))?;
        state.session_add("docs".to_owned(), stdio("session"))?;
        std::fs::write(
            norn.join("settings.json"),
            r#"{"mcp_servers":{"docs":{"command":"project-reloaded"}}}"#,
        )?;

        assert!(state.reload_disk()?);
        assert_eq!(
            state
                .definitions(McpConfigLayer::Cli)
                .and_then(|definitions| definitions.get("docs"))
                .and_then(|definition| definition.command.as_deref()),
            Some("cli"),
        );
        assert!(matches!(
            state.session_entries().get("docs"),
            Some(McpSessionEntry::Definition(definition))
                if definition.command.as_deref() == Some("session")
        ));
        assert_eq!(
            state
                .definitions(McpConfigLayer::SharedProject)
                .and_then(|definitions| definitions.get("docs"))
                .and_then(|definition| definition.command.as_deref()),
            Some("project-reloaded"),
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

#[test]
fn state_debug_does_not_disclose_definition_secrets() -> Result<(), ConfigError> {
    let secret = "mcp-live-state-secret-sentinel";
    let definition = McpServerSettings {
        command: Some("server".to_owned()),
        env: Some(BTreeMap::from([("TOKEN".to_owned(), secret.to_owned())])),
        ..McpServerSettings::default()
    };
    let state = McpConfigState::from_layers(
        PathBuf::from("/project"),
        [
            BTreeMap::from([("docs".to_owned(), definition)]),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        ],
        BTreeMap::new(),
    )?;

    assert!(!format!("{state:?}").contains(secret));
    assert!(!format!("{:?}", state.snapshot()?).contains(secret));
    Ok(())
}

#[test]
fn session_enable_keeps_an_already_enabled_project_definition_unchanged() -> Result<(), ConfigError>
{
    let mut state = McpConfigState::from_layers(
        PathBuf::from("/project"),
        [
            BTreeMap::new(),
            one("project"),
            BTreeMap::new(),
            BTreeMap::new(),
        ],
        BTreeMap::new(),
    )?;

    assert!(!state.session_enable("docs")?);
    let effective = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert_eq!(effective.source(), McpConfigLayer::SharedProject);
    assert!(state.session_entries().is_empty());
    Ok(())
}

#[test]
fn session_enable_can_override_a_disabled_trusted_definition() -> Result<(), ConfigError> {
    let mut disabled = stdio("user");
    disabled.enabled = Some(false);
    let mut state = McpConfigState::from_layers(
        PathBuf::from("/project"),
        [
            BTreeMap::from([("docs".to_owned(), disabled)]),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        ],
        BTreeMap::new(),
    )?;

    assert!(state.session_enable("docs")?);
    let effective = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert_eq!(effective.source(), McpConfigLayer::User);
    assert!(effective.enabled());
    assert_eq!(effective.definition().command.as_deref(), Some("user"));
    assert_eq!(
        state.session_entries().get("docs"),
        Some(&McpSessionEntry::EnabledInherited)
    );

    assert!(state.session_remove("docs")?);
    let revealed = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or_else(|| missing_server("docs", "inspect"))?;
    assert_eq!(revealed.source(), McpConfigLayer::User);
    assert!(!revealed.enabled());
    Ok(())
}
