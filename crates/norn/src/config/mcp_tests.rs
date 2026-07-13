use std::collections::BTreeMap;

use super::*;

fn stdio(command: &str) -> McpServerSettings {
    McpServerSettings {
        command: Some(command.to_owned()),
        ..McpServerSettings::default()
    }
}

#[test]
fn later_layer_replaces_whole_definition_and_keeps_source() -> Result<(), ConfigError> {
    let layers = LoadedSettings {
        user: NornSettings {
            mcp_servers: Some(BTreeMap::from([("docs".to_owned(), stdio("user"))])),
            ..NornSettings::default()
        },
        project: NornSettings {
            mcp_servers: Some(BTreeMap::from([("docs".to_owned(), stdio("project"))])),
            ..NornSettings::default()
        },
        local: NornSettings {
            mcp_servers: Some(BTreeMap::from([("docs".to_owned(), stdio("local"))])),
            ..NornSettings::default()
        },
    };
    let overrides = McpRuntimeOverrides {
        cli: BTreeMap::from([("docs".to_owned(), stdio("cli"))]),
        session: BTreeMap::from([("docs".to_owned(), stdio("session"))]),
    };
    let private_local = NornSettings {
        mcp_servers: Some(BTreeMap::from([(
            "docs".to_owned(),
            stdio("private-local"),
        )])),
        ..NornSettings::default()
    };

    let resolved = resolve_layers(&layers, &private_local, &overrides)?;
    let server = resolved
        .get("docs")
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: "missing resolved test server".to_owned(),
        })?;

    assert_eq!(server.source(), McpConfigSource::Session);
    assert_eq!(server.definition().command.as_deref(), Some("session"));
    assert!(server.definition().args.is_none());
    Ok(())
}

#[test]
fn disabled_entry_masks_lower_definition() -> Result<(), ConfigError> {
    let layers = LoadedSettings {
        user: NornSettings {
            mcp_servers: Some(BTreeMap::from([("docs".to_owned(), stdio("user"))])),
            ..NornSettings::default()
        },
        ..LoadedSettings::default()
    };
    let overrides = McpRuntimeOverrides {
        session: BTreeMap::from([(
            "docs".to_owned(),
            McpServerSettings {
                enabled: Some(false),
                ..McpServerSettings::default()
            },
        )]),
        ..McpRuntimeOverrides::default()
    };

    let resolved = resolve_layers(&layers, &NornSettings::default(), &overrides)?;
    let server = resolved
        .get("docs")
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: "missing disabled test server".to_owned(),
        })?;

    assert_eq!(server.source(), McpConfigSource::Session);
    assert!(!server.enabled());
    Ok(())
}

#[test]
fn normalized_empty_collections_have_same_fingerprint() -> Result<(), ConfigError> {
    let absent = stdio("server");
    let explicit = McpServerSettings {
        command: Some("server".to_owned()),
        args: Some(Vec::new()),
        env: Some(BTreeMap::new()),
        ..McpServerSettings::default()
    };

    assert_eq!(
        fingerprint("docs", &absent)?,
        fingerprint("docs", &explicit)?
    );
    Ok(())
}

#[test]
fn operational_change_changes_fingerprint() -> Result<(), ConfigError> {
    assert_ne!(
        fingerprint("docs", &stdio("first"))?,
        fingerprint("docs", &stdio("second"))?
    );
    Ok(())
}

#[test]
fn resolved_debug_omits_secret_values() -> Result<(), ConfigError> {
    let definition = McpServerSettings {
        command: Some("server".to_owned()),
        env: Some(BTreeMap::from([(
            "TOKEN".to_owned(),
            "environment-secret-sentinel".to_owned(),
        )])),
        ..McpServerSettings::default()
    };
    let server = ResolvedMcpServer {
        name: "docs".to_owned(),
        source: McpConfigSource::User,
        fingerprint: fingerprint("docs", &definition)?,
        definition,
    };

    let rendered = format!("{server:?}");

    assert!(!rendered.contains("environment-secret-sentinel"));
    Ok(())
}
