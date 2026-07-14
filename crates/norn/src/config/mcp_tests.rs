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
fn settings_local_definition_keeps_direct_local_provenance() -> Result<(), ConfigError> {
    let layers = LoadedSettings {
        local: NornSettings {
            mcp_servers: Some(BTreeMap::from([("docs".to_owned(), stdio("local"))])),
            ..NornSettings::default()
        },
        ..LoadedSettings::default()
    };

    let resolved = resolve_layers(
        &layers,
        &NornSettings::default(),
        &McpRuntimeOverrides::default(),
    )?;
    let server = resolved
        .get("docs")
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: "missing local test server".to_owned(),
        })?;

    assert_eq!(server.source(), McpConfigSource::Local);
    assert_eq!(server.definition().command.as_deref(), Some("local"));
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
fn connection_bounds_are_normalized_and_change_fingerprint() -> Result<(), ConfigError> {
    let absent = stdio("server");
    let explicit_default = McpServerSettings {
        command: Some("server".to_owned()),
        max_inbound_message_bytes: Some(DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES),
        ..McpServerSettings::default()
    };
    let bounded_message = McpServerSettings {
        command: Some("server".to_owned()),
        max_inbound_message_bytes: Some(1024),
        ..McpServerSettings::default()
    };
    let bounded_time = McpServerSettings {
        command: Some("server".to_owned()),
        request_timeout_ms: Some(2500),
        ..McpServerSettings::default()
    };

    assert_eq!(
        fingerprint("docs", &absent)?,
        fingerprint("docs", &explicit_default)?,
    );
    assert_ne!(
        fingerprint("docs", &absent)?,
        fingerprint("docs", &bounded_message)?,
    );
    assert_ne!(
        fingerprint("docs", &absent)?,
        fingerprint("docs", &bounded_time)?,
    );
    Ok(())
}

#[test]
fn client_config_resolves_default_and_explicit_connection_bounds()
-> Result<(), Box<dyn std::error::Error>> {
    let default_definition = stdio("server");
    let default_server = ResolvedMcpServer {
        name: "default".to_owned(),
        source: McpConfigSource::User,
        fingerprint: fingerprint("default", &default_definition)?,
        definition: default_definition,
    };
    let default_config = default_server
        .client_config(std::path::Path::new("/workspace"))?
        .ok_or("enabled default MCP server did not produce a client config")?;
    assert_eq!(
        default_config.max_inbound_message_bytes,
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
    );
    assert_eq!(default_config.request_timeout_ms, None);

    let explicit_definition = McpServerSettings {
        command: Some("server".to_owned()),
        max_inbound_message_bytes: Some(4096),
        request_timeout_ms: Some(1250),
        ..McpServerSettings::default()
    };
    let explicit_server = ResolvedMcpServer {
        name: "explicit".to_owned(),
        source: McpConfigSource::User,
        fingerprint: fingerprint("explicit", &explicit_definition)?,
        definition: explicit_definition,
    };
    let explicit_config = explicit_server
        .client_config(std::path::Path::new("/workspace"))?
        .ok_or("enabled explicit MCP server did not produce a client config")?;
    assert_eq!(explicit_config.max_inbound_message_bytes, 4096);
    assert_eq!(explicit_config.request_timeout_ms, Some(1250));
    Ok(())
}

#[test]
fn zero_connection_bounds_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
    for definition in [
        McpServerSettings {
            command: Some("server".to_owned()),
            max_inbound_message_bytes: Some(0),
            ..McpServerSettings::default()
        },
        McpServerSettings {
            command: Some("server".to_owned()),
            request_timeout_ms: Some(0),
            ..McpServerSettings::default()
        },
    ] {
        let error = validate_one("docs", &definition)
            .err()
            .ok_or("zero-valued MCP connection bound was accepted")?;
        assert!(error.to_string().contains("must be positive"));
    }
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
        max_inbound_message_bytes: Some(8192),
        request_timeout_ms: Some(3000),
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
    assert!(rendered.contains("max_inbound_message_bytes: Some(8192)"));
    assert!(rendered.contains("request_timeout_ms: Some(3000)"));
    Ok(())
}
