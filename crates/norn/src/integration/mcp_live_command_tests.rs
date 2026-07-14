use super::*;

fn parse(arguments: &str) -> Result<LiveMcpCommand, LiveMcpCommandError> {
    parse_live_mcp_command(arguments)
}

#[test]
fn parses_every_non_add_operation() -> Result<(), LiveMcpCommandError> {
    assert_eq!(parse("help")?, LiveMcpCommand::Help);
    assert_eq!(parse("list")?, LiveMcpCommand::List);
    assert_eq!(
        parse("inspect docs")?,
        LiveMcpCommand::Inspect {
            name: "docs".to_owned()
        }
    );
    assert_eq!(
        parse("remove docs")?,
        LiveMcpCommand::Remove {
            name: "docs".to_owned()
        }
    );
    assert_eq!(
        parse("enable docs")?,
        LiveMcpCommand::Enable {
            name: "docs".to_owned()
        }
    );
    assert_eq!(
        parse("disable docs")?,
        LiveMcpCommand::Disable {
            name: "docs".to_owned()
        }
    );
    assert_eq!(
        parse("approve docs")?,
        LiveMcpCommand::Approve {
            name: "docs".to_owned()
        }
    );
    assert_eq!(
        parse("revoke docs")?,
        LiveMcpCommand::Revoke {
            name: "docs".to_owned()
        }
    );
    assert_eq!(parse("reload")?, LiveMcpCommand::Reload);
    Ok(())
}

#[test]
fn parses_stdio_secrets_and_argument_boundary() -> Result<(), LiveMcpCommandError> {
    let parsed = parse("add docs stdio command --env TOKEN=secret first -- --env literal")?;
    let LiveMcpCommand::Add { name, definition } = parsed else {
        return Err(LiveMcpCommandError::Usage);
    };
    assert_eq!(name, "docs");
    assert_eq!(definition.command.as_deref(), Some("command"));
    assert_eq!(
        definition.args,
        Some(vec![
            "first".to_owned(),
            "--env".to_owned(),
            "literal".to_owned()
        ])
    );
    assert_eq!(
        definition.env.as_ref().and_then(|env| env.get("TOKEN")),
        Some(&"secret".to_owned())
    );
    Ok(())
}

#[test]
fn parses_http_header_without_exposing_it_in_debug() -> Result<(), LiveMcpCommandError> {
    let parsed = parse(
        "add remote http 'https://example.test/private' --header 'Authorization=Bearer secret'",
    )?;
    let debug = format!("{parsed:?}");
    assert!(!debug.contains("private"));
    assert!(!debug.contains("Bearer secret"));
    assert!(debug.contains("url_present"));
    assert!(debug.contains("header_entries"));
    Ok(())
}

#[test]
fn malformed_secret_input_is_not_echoed() {
    let error = parse("add remote http https://example.test/token --header super-secret");
    let rendered = error.map_or_else(|error| error.to_string(), |_| String::new());
    assert!(!rendered.contains("super-secret"));
    assert!(!rendered.contains("token"));
}

#[test]
fn duplicate_secret_keys_are_rejected_without_echoing_values() {
    let error =
        parse("add remote http https://example.test --header TOKEN=first --header TOKEN=second");
    let rendered = error.map_or_else(|error| error.to_string(), |_| String::new());
    assert!(!rendered.contains("first"));
    assert!(!rendered.contains("second"));
    assert!(rendered.contains("invalid /mcp command"));
}

#[test]
fn empty_secret_values_are_valid() -> Result<(), LiveMcpCommandError> {
    let parsed = parse("add docs stdio command --env OPTIONAL=")?;
    let LiveMcpCommand::Add { definition, .. } = parsed else {
        return Err(LiveMcpCommandError::Usage);
    };
    assert_eq!(
        definition.env.as_ref().and_then(|env| env.get("OPTIONAL")),
        Some(&String::new())
    );
    Ok(())
}

#[test]
fn help_distinguishes_ephemeral_and_persistent_operations() {
    let help = LIVE_MCP_HELP.join("\n");
    assert!(help.contains("session-scoped"));
    assert!(help.contains("approval ledger"));
    assert!(help.contains("rereads disk"));
}

#[test]
fn quoted_arguments_and_escapes_are_preserved() -> Result<(), LiveMcpCommandError> {
    let parsed = parse("add docs stdio command 'two words' three\\ words")?;
    let LiveMcpCommand::Add { definition, .. } = parsed else {
        return Err(LiveMcpCommandError::Usage);
    };
    assert_eq!(
        definition.args,
        Some(vec!["two words".to_owned(), "three words".to_owned()])
    );
    Ok(())
}

#[test]
fn inspect_renderer_redacts_definition_values() {
    let definition = McpServerSettings {
        transport: Some("http".to_owned()),
        url: Some(
            "https://user:password@example.test/private?token=query-secret#frag-secret".to_owned(),
        ),
        headers: Some(BTreeMap::from([(
            "Authorization".to_owned(),
            "Bearer secret".to_owned(),
        )])),
        ..McpServerSettings::default()
    };
    let summary = definition_summary(&definition);
    assert_eq!(
        summary,
        "transport=http target=https://example.test/<redacted-path> args=0 env=0 headers=1 enabled=true"
    );
    assert!(summary.contains("https://example.test"));
    assert!(!summary.contains("private"));
    assert!(!summary.contains("secret"));
    assert!(!summary.contains("password"));
}

#[test]
fn inspect_renderer_shows_only_executable_file_name() {
    let definition = McpServerSettings {
        transport: Some("stdio".to_owned()),
        command: Some("/private/secret/bin/docs-server".to_owned()),
        env: Some(BTreeMap::from([("TOKEN".to_owned(), "secret".to_owned())])),
        ..McpServerSettings::default()
    };
    let summary = definition_summary(&definition);
    assert!(summary.contains("target=docs-server"));
    assert!(!summary.contains("/private/secret"));
    assert!(!summary.contains("TOKEN"));
    assert!(!summary.contains("secret"));
}

#[test]
fn history_policy_recognises_malformed_definition_adds() {
    assert!(is_live_mcp_definition_input(
        " /MCP   ADD remote http broken --header TOKEN=secret"
    ));
    assert!(!is_live_mcp_definition_input("/mcp list"));
    assert!(!is_live_mcp_definition_input("explain /mcp add"));
}

#[tokio::test]
async fn unavailable_handle_is_a_safe_typed_failure() -> Result<(), LiveMcpCommandError> {
    let error = execute_live_mcp_command(None, LiveMcpCommand::List)
        .await
        .err()
        .ok_or(LiveMcpCommandError::Usage)?;
    assert_eq!(error, LiveMcpCommandError::Unavailable);
    Ok(())
}
