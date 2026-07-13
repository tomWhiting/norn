//! MCP extension URI collection for the Norn CLI (NC-004 R6).
//!
//! NC-004's responsibility is to collect every `-e` / `--extension` URI
//! into a validated `Vec<String>` that the tool-registry construction
//! later in the pipeline (NC-003 or a later brief) hands off to
//! [`norn::integration::mcp_client::McpClient::connect`]. URI parsing into
//! a [`McpTransport`](norn::integration::mcp_client::McpTransport) is
//! intentionally out of scope for this brief — see the deferred-parsing
//! note in the brief's R6 scout context.

use std::collections::BTreeMap;

use norn::config::McpServerSettings;

use crate::cli::BuildError;

/// Validate and collect the `-e` URIs into an owned `Vec<String>`.
///
/// Empty URIs are rejected per the brief acceptance: `-e ""` returns an
/// error. The function copies its input rather than borrowing so the
/// resulting bundle is independent of the [`crate::cli::Cli`] lifetime.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when any URI is empty after trimming.
pub fn collect_extension_uris(uris: &[String]) -> Result<Vec<String>, BuildError> {
    let mut out: Vec<String> = Vec::with_capacity(uris.len());
    for (index, uri) in uris.iter().enumerate() {
        let trimmed = uri.trim();
        if trimmed.is_empty() {
            return Err(BuildError::Argument(format!(
                "extension URI at position {index} is empty (--extension requires a non-empty URI)",
            )));
        }
        out.push(trimmed.to_owned());
    }
    Ok(out)
}

/// Parse CLI extensions into named MCP definitions. `NAME=URI` supplies an
/// explicit name; a naked URI receives a deterministic `extension_N` name.
pub fn collect_extension_servers(
    values: &[String],
) -> Result<BTreeMap<String, McpServerSettings>, BuildError> {
    let uris = collect_extension_uris(values)?;
    let mut servers = BTreeMap::new();
    for (index, raw) in uris.iter().enumerate() {
        let generated_name = format!("extension_{}", index + 1);
        let (name, uri) = explicit_name(raw).unwrap_or((&generated_name, raw.as_str()));
        if servers.contains_key(name) {
            return Err(BuildError::Argument(format!(
                "MCP extension name '{name}' is specified more than once",
            )));
        }
        servers.insert(name.to_owned(), server_from_uri(name, uri)?);
    }
    Ok(servers)
}

fn explicit_name(value: &str) -> Option<(&str, &str)> {
    let (name, uri) = value.split_once('=')?;
    let valid_name = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
    (valid_name && uri.contains("://")).then_some((name, uri))
}

fn server_from_uri(name: &str, uri: &str) -> Result<McpServerSettings, BuildError> {
    let (scheme, target) = uri.split_once("://").ok_or_else(|| {
        BuildError::Argument(format!(
            "MCP extension '{name}' must use stdio://, http://, or https://",
        ))
    })?;
    match scheme {
        "stdio" if !target.is_empty() => Ok(McpServerSettings {
            transport: Some("stdio".to_owned()),
            command: Some(target.to_owned()),
            ..McpServerSettings::default()
        }),
        "http" | "https" => Ok(McpServerSettings {
            transport: Some("http".to_owned()),
            url: Some(uri.to_owned()),
            ..McpServerSettings::default()
        }),
        "stdio" => Err(BuildError::Argument(format!(
            "MCP extension '{name}' has an empty stdio command",
        ))),
        _ => Err(BuildError::Argument(format!(
            "MCP extension '{name}' uses unsupported scheme '{scheme}'",
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_vec() {
        let result = collect_extension_uris(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn collects_multiple_uris_in_order() {
        let uris = vec![
            "stdio://path/to/server".to_owned(),
            "http://localhost:3000".to_owned(),
        ];
        let result = collect_extension_uris(&uris).unwrap();
        assert_eq!(
            result,
            vec!["stdio://path/to/server", "http://localhost:3000"]
        );
    }

    #[test]
    fn empty_uri_returns_argument_error() {
        let err = collect_extension_uris(&[String::new()]).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("position 0"), "reason: {reason}");
            }
            other @ BuildError::Auth(_) => panic!("expected Argument error, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_only_uri_returns_argument_error() {
        let err = collect_extension_uris(&["   ".to_owned()]).unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }

    #[test]
    fn second_empty_uri_names_correct_position() {
        let err = collect_extension_uris(&[
            "stdio://ok".to_owned(),
            String::new(),
            "http://later".to_owned(),
        ])
        .unwrap_err();
        match err {
            BuildError::Argument(reason) => assert!(reason.contains("position 1")),
            other @ BuildError::Auth(_) => panic!("expected Argument error, got {other:?}"),
        }
    }

    #[test]
    fn named_and_generated_extensions_become_cli_servers() {
        let servers = collect_extension_servers(&[
            "docs=https://example.test/mcp".to_owned(),
            "stdio:///usr/local/bin/browser".to_owned(),
        ])
        .unwrap();

        assert_eq!(
            servers.get("docs").and_then(|server| server.url.as_deref()),
            Some("https://example.test/mcp")
        );
        assert_eq!(
            servers
                .get("extension_2")
                .and_then(|server| server.command.as_deref()),
            Some("/usr/local/bin/browser")
        );
    }

    #[test]
    fn duplicate_explicit_extension_name_is_rejected() {
        let error = collect_extension_servers(&[
            "docs=https://one.example/mcp".to_owned(),
            "docs=https://two.example/mcp".to_owned(),
        ])
        .unwrap_err();

        assert!(error.to_string().contains("specified more than once"));
    }
}
