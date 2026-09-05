//! Launch-document contracts exercised through the public CLI collection seam.

use std::collections::BTreeMap;
use std::error::Error;

use clap::Parser;
use norn::config::McpServerSettings;

use crate::cli::{BuildError, Cli};

use super::{McpConfigArg, collect_mcp_launch_servers};

fn collect(documents: &[&str]) -> Result<BTreeMap<String, McpServerSettings>, BuildError> {
    let configs = documents
        .iter()
        .map(|document| McpConfigArg((*document).to_owned()))
        .collect::<Vec<_>>();
    collect_mcp_launch_servers(&configs, &[])
}

fn refusal(document: &str) -> Result<String, Box<dyn Error>> {
    match collect(&[document]) {
        Ok(_) => Err("invalid launch document was accepted".into()),
        Err(error) => {
            assert!(matches!(error, BuildError::Argument(_)));
            Ok(error.to_string())
        }
    }
}

#[test]
fn cli_accepts_repeated_documents_and_redacts_json_and_paths() -> Result<(), Box<dyn Error>> {
    let raw = r#"{"mcpServers":{"tools":{"command":"SECRET_EXECUTABLE"}}}"#;
    let cli = Cli::try_parse_from([
        "norn",
        "--mcp-config",
        raw,
        "--mcp-config",
        "SECRET_PATH.json",
    ])?;
    assert_eq!(cli.mcp_config.len(), 2);
    let debug = format!("{cli:?}");
    assert!(!debug.contains("SECRET_EXECUTABLE"));
    assert!(!debug.contains("SECRET_PATH"));
    assert!(!debug.contains("mcpServers"));
    assert!(debug.contains("McpConfigArg([REDACTED])"));
    Ok(())
}

#[test]
fn preserves_complete_process_and_http_settings() -> Result<(), Box<dyn Error>> {
    let servers = collect(&[r#"{
        "mcpServers": {
            "process": {
                "type": "stdio", "command": "./fixture",
                "args": ["with spaces", "$(do-not-execute)", "--flag=value"],
                "env": {"TOKEN": "secret", "EMPTY": ""}, "enabled": true,
                "max_inbound_message_bytes": 1234, "request_timeout_ms": 4321
            },
            "remote": {
                "transport": "http", "url": "https://example.test/mcp",
                "headers": {"Authorization": "Bearer secret"}
            }
        }
    }"#])?;
    let process = servers.get("process").ok_or("process definition missing")?;
    assert_eq!(process.transport.as_deref(), Some("stdio"));
    assert_eq!(process.command.as_deref(), Some("./fixture"));
    assert_eq!(
        process.args.as_deref(),
        Some(
            [
                "with spaces".to_owned(),
                "$(do-not-execute)".to_owned(),
                "--flag=value".to_owned()
            ]
            .as_slice()
        ),
    );
    assert_eq!(process.enabled, Some(true));
    assert_eq!(process.max_inbound_message_bytes, Some(1234));
    assert_eq!(process.request_timeout_ms, Some(4321));
    assert_eq!(
        process
            .env
            .as_ref()
            .and_then(|env| env.get("TOKEN"))
            .map(String::as_str),
        Some("secret"),
    );
    let remote = servers.get("remote").ok_or("remote definition missing")?;
    assert_eq!(remote.url.as_deref(), Some("https://example.test/mcp"));
    assert_eq!(
        remote
            .headers
            .as_ref()
            .and_then(|headers| headers.get("Authorization"))
            .map(String::as_str),
        Some("Bearer secret"),
    );
    Ok(())
}

#[test]
fn empty_maps_and_disabled_masks_are_preserved() -> Result<(), Box<dyn Error>> {
    assert!(collect(&[])?.is_empty());
    assert!(collect(&[r#"{"mcpServers":{}}"#])?.is_empty());
    let servers = collect(&[r#"{"mcpServers":{"disabled":{"enabled":false}}}"#])?;
    assert_eq!(
        servers.get("disabled"),
        Some(&McpServerSettings {
            enabled: Some(false),
            ..McpServerSettings::default()
        }),
    );
    Ok(())
}

#[test]
fn disjoint_documents_combine_with_existing_uri_extensions() -> Result<(), Box<dyn Error>> {
    let first: McpConfigArg = r#"{"mcpServers":{"one":{"command":"first"}}}"#.parse()?;
    let second: McpConfigArg = r#"{"mcpServers":{"two":{"command":"second"}}}"#.parse()?;
    let servers = collect_mcp_launch_servers(
        &[first, second],
        &[
            "named=stdio://third".to_owned(),
            "https://example.test/mcp".to_owned(),
        ],
    )?;
    assert_eq!(
        servers.keys().map(String::as_str).collect::<Vec<_>>(),
        ["extension_2", "named", "one", "two"],
    );
    Ok(())
}

#[test]
fn duplicate_decoded_json_keys_are_refused_at_every_object_depth() -> Result<(), Box<dyn Error>> {
    for document in [
        r#"{"mcpServers":{},"mcpServers":{}}"#,
        r#"{"mcpServers":{"same":{"command":"one"},"same":{"command":"two"}}}"#,
        r#"{"mcpServers":{"same":{"command":"one","command":"two"}}}"#,
        r#"{"mcpServers":{"same":{"command":"one","env":{"TOKEN":"one","TOKEN":"two"}}}}"#,
        r#"{"mcpServers":{"same":{"url":"https://example.test","headers":{"X-Key":"one","X-Key":"two"}}}}"#,
        r#"{"mcpServers":{"same":{"command":"one","env":{"KEY":"one","\u004bEY":"two"}}}}"#,
        r#"{"mcpServers":{"same":{"command":"one","unknown":[{"nested":{"key":1,"key":2}}]}}}"#,
    ] {
        assert!(refusal(document)?.contains("duplicate object key"));
    }
    Ok(())
}

#[test]
fn duplicate_server_names_across_flags_and_extensions_are_refused() -> Result<(), Box<dyn Error>> {
    let document = r#"{"mcpServers":{"same":{"command":"one"}}}"#;
    let Err(error) = collect(&[document, document]) else {
        return Err("duplicate names across documents were accepted".into());
    };
    assert!(
        error
            .to_string()
            .contains("document 2 repeats server 'same'")
    );
    let config: McpConfigArg = document.parse()?;
    let Err(error) = collect_mcp_launch_servers(&[config], &["same=stdio://two".to_owned()]) else {
        return Err("document/extension name collision was accepted".into());
    };
    assert!(
        error
            .to_string()
            .contains("document 1 repeats server 'same'")
    );
    assert!(
        collect_mcp_launch_servers(
            &[],
            &["same=stdio://one".to_owned(), "same=stdio://two".to_owned()],
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn strict_envelope_and_existing_field_types_are_required() -> Result<(), Box<dyn Error>> {
    for document in [
        "{}",
        "[]",
        "[{}]",
        r#"[{"server":{"command":"fixture"}}]"#,
        "null",
        "42",
        r#""SECRET""#,
        r#"{"mcpServers":[]}"#,
        r#"{"mcpServers":null}"#,
        r#"{"mcpServers":{"server":[]}}"#,
        r#"{"mcpServers":{"server":[false,null,null,null,null,null,null,null,null]}}"#,
        r#"{"mcp_servers":{}}"#,
        r#"{"mcpServers":{},"unknown":"SECRET"}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","unknown":"SECRET"}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","enabled":"SECRET"}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","args":["ok",{"SECRET":true}]}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","env":{"KEY":42}}}}"#,
        r#"{"mcpServers":{"server":{"url":"https://example.test","headers":{"KEY":false}}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","request_timeout_ms":-1}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","max_inbound_message_bytes":1.5}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","type":"stdio","transport":"stdio"}}}"#,
        r#"{"mcpServers":{}} trailing SECRET"#,
    ] {
        let error = refusal(document)?;
        assert!(!error.contains("SECRET"));
        assert!(error.contains("--mcp-config document 1"));
    }
    Ok(())
}

#[test]
fn shared_mcp_validation_refuses_invalid_settings_without_values() -> Result<(), Box<dyn Error>> {
    for document in [
        r#"{"mcpServers":{"":{"command":"fixture"}}}"#,
        r#"{"mcpServers":{"bad.name":{"command":"fixture"}}}"#,
        r#"{"mcpServers":{"server":{}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","url":"https://example.test"}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","type":"SECRET"}}}"#,
        r#"{"mcpServers":{"server":{"command":" "}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","headers":{"X-Key":"SECRET"}}}}"#,
        r#"{"mcpServers":{"server":{"url":"SECRET"}}}"#,
        r#"{"mcpServers":{"server":{"url":"https://example.test","type":"sse"}}}"#,
        r#"{"mcpServers":{"server":{"url":"https://example.test","env":{"TOKEN":"SECRET"}}}}"#,
        r#"{"mcpServers":{"server":{"url":"https://example.test","headers":{"X-Key":"SECRET\n"}}}}"#,
        r#"{"mcpServers":{"server":{"url":"https://example.test","headers":{"X-Key":"one","x-key":"SECRET"}}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","request_timeout_ms":0}}}"#,
        r#"{"mcpServers":{"server":{"command":"fixture","max_inbound_message_bytes":0}}}"#,
    ] {
        let error = refusal(document)?;
        assert!(error.contains("server entry 1:"));
        assert!(!error.contains("SECRET"));
    }
    Ok(())
}

#[test]
fn file_documents_are_read_without_modification() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.json");
    let contents = r#"{"mcpServers":{"file":{"command":"./fixture","args":["arg"]}}}"#;
    std::fs::write(&path, contents)?;
    let argument: McpConfigArg = path
        .to_str()
        .ok_or("temporary path was not UTF-8")?
        .parse()?;
    let servers = collect_mcp_launch_servers(&[argument], &[])?;
    assert_eq!(
        servers
            .get("file")
            .and_then(|server| server.command.as_deref()),
        Some("./fixture")
    );
    assert_eq!(std::fs::read_to_string(&path)?, contents);
    Ok(())
}

#[test]
fn errors_identify_file_paths_but_withhold_contents_and_uri_values() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("SECRET_PATH.json");
    let argument: McpConfigArg = path
        .to_str()
        .ok_or("temporary path was not UTF-8")?
        .parse()?;
    let Err(error) = collect_mcp_launch_servers(std::slice::from_ref(&argument), &[]) else {
        return Err("missing file was accepted".into());
    };
    assert!(error.to_string().contains("SECRET_PATH.json"));
    assert!(error.to_string().contains("could not be read"));
    std::fs::write(
        &path,
        r#"{"mcpServers":{"server":{"enabled":"SECRET_VALUE"}}}"#,
    )?;
    let Err(error) = collect_mcp_launch_servers(&[argument], &[]) else {
        return Err("invalid file content was accepted".into());
    };
    assert!(!format!("{error:?}").contains("SECRET"));
    let Err(error) = collect_mcp_launch_servers(&[], &["SECRET_SCHEME://target".to_owned()]) else {
        return Err("unsupported extension scheme was accepted".into());
    };
    assert!(!format!("{error:?}").contains("SECRET"));
    Ok(())
}

#[test]
fn errors_keep_server_and_invalid_field_referents() -> Result<(), Box<dyn Error>> {
    let zero_limit =
        refusal(r#"{"mcpServers":{"named":{"command":"fixture","request_timeout_ms":0}}}"#)?;
    assert!(zero_limit.contains("mcp server 'named' request_timeout_ms must be positive"));
    let command = refusal(r#"{"mcpServers":{"named":{"command":" "}}}"#)?;
    assert!(command.contains("mcp_servers.named.command: must not be empty"));
    let transport = refusal(r#"{"mcpServers":{"named":{"command":"fixture","type":"SECRET"}}}"#)?;
    assert!(transport.contains("mcp server 'named' has incompatible or unsupported transport"));
    assert!(!transport.contains("SECRET"));
    let alias = refusal(
        r#"{"mcpServers":{"named":{"command":"fixture","type":"stdio","transport":"stdio"}}}"#,
    )?;
    assert!(alias.contains("duplicate transport/type declaration"));
    let Err(error) = collect_mcp_launch_servers(
        &[],
        &["same=stdio://one".to_owned(), "same=stdio://two".to_owned()],
    ) else {
        return Err("duplicate extensions were accepted".into());
    };
    assert!(
        error
            .to_string()
            .contains("MCP extension name 'same' is specified more than once")
    );
    Ok(())
}

#[test]
fn standard_type_alias_serializes_using_existing_transport_field() -> Result<(), Box<dyn Error>> {
    let settings: McpServerSettings =
        serde_json::from_str(r#"{"type":"stdio","command":"fixture"}"#)?;
    assert_eq!(settings.transport.as_deref(), Some("stdio"));
    let encoded = serde_json::to_string(&settings)?;
    assert!(encoded.contains(r#""transport":"stdio""#));
    assert!(!encoded.contains(r#""type":"#));
    Ok(())
}
