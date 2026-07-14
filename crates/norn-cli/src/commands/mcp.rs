//! `norn mcp …` subcommand dispatchers (NC-008 R11–R12).
//!
//! `mcp serve` exposes the Norn tool catalogue over the standard Model
//! Context Protocol so external agents can drive it. The stateless tool
//! subset is registered — tools that require shared state (skills,
//! tool-search, tasks, LSP, web, agent coordination) are deferred to
//! NC-003 which builds the full runtime registry.
//!
//! `mcp connect` is a probe: it dials a server, performs the
//! `initialize` handshake plus `tools/list`, prints discovery output to
//! stderr, and exits. The connection is closed on Drop.

use std::collections::HashMap;
use std::sync::Arc;

use norn::integration::mcp_client::MCP_PROTOCOL_VERSION;
use norn::integration::mcp_client::{McpClient, McpServerConfig as McpClientConfig, McpTransport};
use norn::integration::mcp_server::{McpServer, McpServerConfig as McpServerOpts};
use norn::tool::registry::ToolRegistry;
use norn::tools::bash::BashTool;
use norn::tools::edit::EditTool;
use norn::tools::patch::ApplyPatchTool;
use norn::tools::read::ReadTool;
use norn::tools::search::SearchTool;
use norn::tools::write::WriteTool;

use super::mcp_config::{ConfigCommand, run_config_command};
use crate::cli::{BuildError, Cli, ExitCode, McpCmd};

/// Top-level dispatcher for `norn mcp`.
pub fn run_mcp(cli: &Cli, cmd: McpCmd) -> ExitCode {
    match cmd {
        McpCmd::Serve => run_serve(),
        McpCmd::Connect { uri } => run_connect(&uri),
        McpCmd::List => run_config_command(cli, ConfigCommand::List),
        McpCmd::Inspect { name } => run_config_command(cli, ConfigCommand::Inspect { name }),
        McpCmd::Add {
            name,
            scope,
            command,
            args,
            url,
            env,
            header,
        } => run_config_command(
            cli,
            ConfigCommand::Add {
                name,
                scope,
                command,
                args,
                url,
                env,
                header,
            },
        ),
        McpCmd::Remove { name, scope } => {
            run_config_command(cli, ConfigCommand::Remove { name, scope })
        }
        McpCmd::Enable { name, scope } => run_config_command(
            cli,
            ConfigCommand::SetEnabled {
                name,
                scope,
                enabled: true,
            },
        ),
        McpCmd::Disable { name, scope } => run_config_command(
            cli,
            ConfigCommand::SetEnabled {
                name,
                scope,
                enabled: false,
            },
        ),
        McpCmd::Approve { name, all } => {
            run_config_command(cli, ConfigCommand::Approve { name, all })
        }
        McpCmd::Revoke { name, all } => {
            run_config_command(cli, ConfigCommand::Revoke { name, all })
        }
    }
}

// ---------------------------------------------------------------------------
// R11: serve
// ---------------------------------------------------------------------------

fn run_serve() -> ExitCode {
    let registry = build_stateless_registry();
    let config = McpServerOpts {
        transport: McpTransport::Stdio {
            command: String::new(),
            args: Vec::new(),
        },
        server_name: "norn".to_owned(),
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let server = McpServer::new(Arc::new(registry), config);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("norn: failed to create tokio runtime: {err}");
            return ExitCode::AgentError;
        }
    };
    match rt.block_on(server.serve_stdio()) {
        Ok(()) => ExitCode::Success,
        Err(err) => {
            eprintln!("norn: MCP server I/O error: {err}");
            ExitCode::AgentError
        }
    }
}

fn build_stateless_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(BashTool::new()));
    registry.register(Box::new(ReadTool::new()));
    registry.register(Box::new(WriteTool::new()));
    registry.register(Box::new(EditTool::new()));
    registry.register(Box::new(build_apply_patch_tool()));
    registry.register(Box::new(SearchTool::new()));
    registry
}

/// Construct the `apply_patch` tool, wiring libyggd-backed tier-1 entity
/// resolution when the `libyggd-ast` feature is enabled and falling back to
/// the extractor-less tool otherwise.
#[cfg(feature = "libyggd-ast")]
fn build_apply_patch_tool() -> ApplyPatchTool {
    ApplyPatchTool::with_extractor(Arc::new(norn::tools::LibygdEntityExtractor))
}

/// Extractor-less `apply_patch` for builds without the `libyggd-ast` feature:
/// tier 1 is skipped and resolution starts at the context-anchored tier 2.
#[cfg(not(feature = "libyggd-ast"))]
fn build_apply_patch_tool() -> ApplyPatchTool {
    ApplyPatchTool::new()
}

// ---------------------------------------------------------------------------
// R12: connect
// ---------------------------------------------------------------------------

fn run_connect(uri: &str) -> ExitCode {
    let transport = match parse_transport(uri) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("norn: {err}");
            return err.exit_code();
        }
    };

    let config = McpClientConfig {
        name: "probe".to_owned(),
        transport,
        env: HashMap::new(),
        headers: HashMap::new(),
        working_dir: None,
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("norn: failed to create tokio runtime: {err}");
            return ExitCode::AgentError;
        }
    };

    match rt.block_on(McpClient::connect(config)) {
        Ok(client) => {
            eprintln!("Server name: {}", client.name());
            eprintln!("Protocol: {MCP_PROTOCOL_VERSION}");
            for tool in client.tools() {
                let desc = if tool.description.is_empty() {
                    "(no description)"
                } else {
                    tool.description.as_str()
                };
                eprintln!("Tool: {} — {desc}", tool.name);
            }
            ExitCode::Success
        }
        Err(err) => {
            eprintln!("MCP connect failed: {err}");
            ExitCode::AgentError
        }
    }
}

fn parse_transport(uri: &str) -> Result<McpTransport, BuildError> {
    let (scheme, rest) = uri.split_once("://").ok_or_else(|| {
        BuildError::Argument(format!(
            "unsupported MCP scheme: {uri}; expected stdio:// or http(s)://"
        ))
    })?;

    match scheme {
        "stdio" => {
            if rest.is_empty() {
                return Err(BuildError::Argument(
                    "stdio:// URI requires a command path".to_owned(),
                ));
            }
            Ok(McpTransport::Stdio {
                command: rest.to_owned(),
                args: Vec::new(),
            })
        }
        "http" | "https" => Ok(McpTransport::Http {
            url: uri.to_owned(),
        }),
        other => Err(BuildError::Argument(format!(
            "unsupported MCP scheme: {other}; expected stdio:// or http(s)://"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_transport_stdio_captures_command() {
        let t = parse_transport("stdio:///usr/local/bin/server").unwrap();
        match t {
            McpTransport::Stdio { command, args } => {
                assert_eq!(command, "/usr/local/bin/server");
                assert!(args.is_empty());
            }
            other @ McpTransport::Http { .. } => panic!("expected stdio, got {other:?}"),
        }
    }

    #[test]
    fn parse_transport_http_captures_full_url() {
        let t = parse_transport("https://example.com/mcp").unwrap();
        match t {
            McpTransport::Http { url } => assert_eq!(url, "https://example.com/mcp"),
            other @ McpTransport::Stdio { .. } => panic!("expected http, got {other:?}"),
        }
    }

    #[test]
    fn parse_transport_unsupported_scheme_is_argument_error() {
        let err = parse_transport("foo://bar").unwrap_err();
        assert_eq!(err.exit_code(), ExitCode::ArgumentError);
    }

    #[test]
    fn parse_transport_missing_scheme_is_argument_error() {
        let err = parse_transport("just-a-string").unwrap_err();
        assert_eq!(err.exit_code(), ExitCode::ArgumentError);
    }

    #[test]
    fn parse_transport_empty_stdio_rejected() {
        let err = parse_transport("stdio://").unwrap_err();
        assert_eq!(err.exit_code(), ExitCode::ArgumentError);
    }

    #[test]
    fn build_stateless_registry_contains_expected_tools() {
        let registry = build_stateless_registry();
        let names: Vec<&str> = registry.names().collect();
        for expected in ["bash", "read", "write", "edit", "apply_patch", "search"] {
            assert!(
                names.contains(&expected),
                "missing tool {expected} in registry; got {names:?}"
            );
        }
    }
}
