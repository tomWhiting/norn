//! MCP extension URI collection for the Norn CLI (NC-004 R6).
//!
//! NC-004's responsibility is to collect every `-e` / `--extension` URI
//! into a validated `Vec<String>` that the tool-registry construction
//! later in the pipeline (NC-003 or a later brief) hands off to
//! [`norn::integration::mcp_client::McpClient::connect`]. URI parsing into
//! a [`McpTransport`](norn::integration::mcp_client::McpTransport) is
//! intentionally out of scope for this brief — see the deferred-parsing
//! note in the brief's R6 scout context.

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
}
