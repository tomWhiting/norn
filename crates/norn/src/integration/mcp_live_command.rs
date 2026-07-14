//! Shared parsing, execution, and redacted rendering for live `/mcp` commands.

use std::collections::BTreeMap;

use crate::config::{McpApprovalState, McpConfigLayer, McpLayerEntry, McpServerSettings};

use super::mcp_runtime::McpRuntimeServerState;
use super::{McpControlError, McpControlHandle};

/// Session-scoped MCP operation accepted by interactive command surfaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveMcpCommand {
    /// Show the shared session-scoped command grammar.
    Help,
    /// List effective servers and their current activation state.
    List,
    /// Inspect redacted provenance for one server.
    Inspect {
        /// Logical configured server name.
        name: String,
    },
    /// Add or replace one ephemeral session definition.
    Add {
        /// Logical configured server name.
        name: String,
        /// Complete ephemeral replacement definition.
        definition: McpServerSettings,
    },
    /// Remove only the session-layer entry.
    Remove {
        /// Logical configured server name.
        name: String,
    },
    /// Enable a disabled session entry.
    Enable {
        /// Logical configured server name.
        name: String,
    },
    /// Disable the effective definition through a session tombstone.
    Disable {
        /// Logical configured server name.
        name: String,
    },
    /// Approve the effective project-controlled definition.
    Approve {
        /// Logical configured server name.
        name: String,
    },
    /// Revoke remembered approval for one project server name.
    Revoke {
        /// Logical configured server name.
        name: String,
    },
    /// Reload disk-backed layers without discarding session changes.
    Reload,
}

/// Safe command parse or execution failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LiveMcpCommandError {
    /// The command does not match the documented grammar.
    #[error("invalid /mcp command; use /mcp help for session-scoped command syntax")]
    Usage,
    /// No live MCP control plane was installed for this agent.
    #[error("live MCP control is unavailable for this agent")]
    Unavailable,
    /// The control plane rejected or could not complete the operation.
    #[error("{0}")]
    Control(McpControlError),
}

/// Parse the argument tail following `/mcp`.
pub fn parse_live_mcp_command(arguments: &str) -> Result<LiveMcpCommand, LiveMcpCommandError> {
    let words = tokenize(arguments)?;
    let Some(command) = words.first().map(String::as_str) else {
        return Err(LiveMcpCommandError::Usage);
    };
    match command.to_ascii_lowercase().as_str() {
        "help" if words.len() == 1 => Ok(LiveMcpCommand::Help),
        "list" if words.len() == 1 => Ok(LiveMcpCommand::List),
        "inspect" => named_command(&words, |name| LiveMcpCommand::Inspect { name }),
        "remove" => named_command(&words, |name| LiveMcpCommand::Remove { name }),
        "enable" => named_command(&words, |name| LiveMcpCommand::Enable { name }),
        "disable" => named_command(&words, |name| LiveMcpCommand::Disable { name }),
        "approve" => named_command(&words, |name| LiveMcpCommand::Approve { name }),
        "revoke" => named_command(&words, |name| LiveMcpCommand::Revoke { name }),
        "reload" if words.len() == 1 => Ok(LiveMcpCommand::Reload),
        "add" => parse_add(&words),
        _ => Err(LiveMcpCommandError::Usage),
    }
}

/// Whether a complete input is a live definition-add command that must not
/// enter persistent prompt history. This intentionally recognises malformed
/// add commands too, before any secret-bearing parser error can occur.
#[must_use]
pub fn is_live_mcp_definition_input(input: &str) -> bool {
    let mut words = input.split_whitespace();
    words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("/mcp"))
        && words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("add"))
}

fn named_command(
    words: &[String],
    build: impl FnOnce(String) -> LiveMcpCommand,
) -> Result<LiveMcpCommand, LiveMcpCommandError> {
    if words.len() != 2 || words[1].is_empty() {
        return Err(LiveMcpCommandError::Usage);
    }
    Ok(build(words[1].clone()))
}

fn parse_add(words: &[String]) -> Result<LiveMcpCommand, LiveMcpCommandError> {
    if words.len() < 4 || words[1].is_empty() {
        return Err(LiveMcpCommandError::Usage);
    }
    let name = words[1].clone();
    let definition = match words[2].to_ascii_lowercase().as_str() {
        "stdio" => parse_stdio(&words[3..])?,
        "http" => parse_http(&words[3..])?,
        _ => return Err(LiveMcpCommandError::Usage),
    };
    Ok(LiveMcpCommand::Add { name, definition })
}

fn parse_stdio(words: &[String]) -> Result<McpServerSettings, LiveMcpCommandError> {
    let Some(command) = words.first().filter(|word| !word.is_empty()) else {
        return Err(LiveMcpCommandError::Usage);
    };
    let (args, env) = parse_options(&words[1..], "--env")?;
    Ok(McpServerSettings {
        transport: Some("stdio".to_owned()),
        command: Some(command.clone()),
        args: (!args.is_empty()).then_some(args),
        env: (!env.is_empty()).then_some(env),
        ..McpServerSettings::default()
    })
}

fn parse_http(words: &[String]) -> Result<McpServerSettings, LiveMcpCommandError> {
    let Some(url) = words.first().filter(|word| !word.is_empty()) else {
        return Err(LiveMcpCommandError::Usage);
    };
    let (extras, headers) = parse_options(&words[1..], "--header")?;
    if !extras.is_empty() {
        return Err(LiveMcpCommandError::Usage);
    }
    Ok(McpServerSettings {
        transport: Some("http".to_owned()),
        url: Some(url.clone()),
        headers: (!headers.is_empty()).then_some(headers),
        ..McpServerSettings::default()
    })
}

fn parse_options(
    words: &[String],
    option: &str,
) -> Result<(Vec<String>, BTreeMap<String, String>), LiveMcpCommandError> {
    let mut plain = Vec::new();
    let mut entries = BTreeMap::new();
    let mut index = 0;
    while index < words.len() {
        if words[index] == "--" {
            plain.extend_from_slice(&words[index + 1..]);
            break;
        }
        if words[index] == option {
            let Some(entry) = words.get(index + 1) else {
                return Err(LiveMcpCommandError::Usage);
            };
            let Some((key, value)) = entry.split_once('=') else {
                return Err(LiveMcpCommandError::Usage);
            };
            if key.is_empty() {
                return Err(LiveMcpCommandError::Usage);
            }
            if entries.insert(key.to_owned(), value.to_owned()).is_some() {
                return Err(LiveMcpCommandError::Usage);
            }
            index += 2;
        } else {
            plain.push(words[index].clone());
            index += 1;
        }
    }
    Ok((plain, entries))
}

fn tokenize(input: &str) -> Result<Vec<String>, LiveMcpCommandError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            started = true;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
                started = true;
            } else {
                current.push(character);
            }
        } else if character.is_whitespace() && quote.is_none() {
            if started {
                words.push(std::mem::take(&mut current));
                started = false;
            }
        } else {
            current.push(character);
            started = true;
        }
    }
    if escaped || quote.is_some() {
        return Err(LiveMcpCommandError::Usage);
    }
    if started {
        words.push(current);
    }
    Ok(words)
}

/// Execute one parsed command and return only redacted operator-facing lines.
pub async fn execute_live_mcp_command(
    handle: Option<&McpControlHandle>,
    command: LiveMcpCommand,
) -> Result<Vec<String>, LiveMcpCommandError> {
    if command == LiveMcpCommand::Help {
        return Ok(LIVE_MCP_HELP.iter().map(ToString::to_string).collect());
    }
    let handle = handle.ok_or(LiveMcpCommandError::Unavailable)?;
    match command {
        LiveMcpCommand::Help => Ok(Vec::new()),
        LiveMcpCommand::List => Ok(render_list(handle.list().await.map_err(control)?)),
        LiveMcpCommand::Inspect { name } => {
            let details = handle.inspect(name).await.map_err(control)?;
            Ok(render_details(&details))
        }
        LiveMcpCommand::Add { name, definition } => Ok(mutation_lines(
            "session definition updated",
            "session-scoped",
            &handle
                .session_add(name, definition)
                .await
                .map_err(control)?,
        )),
        LiveMcpCommand::Remove { name } => Ok(mutation_lines(
            "session definition removed",
            "session-scoped",
            &handle.session_remove(name).await.map_err(control)?,
        )),
        LiveMcpCommand::Enable { name } => Ok(mutation_lines(
            "session definition enabled",
            "session-scoped",
            &handle.session_enable(name).await.map_err(control)?,
        )),
        LiveMcpCommand::Disable { name } => Ok(mutation_lines(
            "session definition disabled",
            "session-scoped",
            &handle.session_disable(name).await.map_err(control)?,
        )),
        LiveMcpCommand::Approve { name } => Ok(mutation_lines(
            "project definition approved",
            "project approval persisted",
            &handle.approve(name).await.map_err(control)?,
        )),
        LiveMcpCommand::Revoke { name } => Ok(mutation_lines(
            "project definition approval revoked",
            "project approval persisted",
            &handle.revoke(name).await.map_err(control)?,
        )),
        LiveMcpCommand::Reload => Ok(mutation_lines(
            "disk-backed MCP settings reloaded",
            "session overrides retained",
            &handle.reload().await.map_err(control)?,
        )),
    }
}

fn control(error: McpControlError) -> LiveMcpCommandError {
    LiveMcpCommandError::Control(error)
}

fn mutation_lines(action: &str, scope: &str, result: &super::McpMutationResult) -> Vec<String> {
    let outcome = if result.changed {
        "changed"
    } else {
        "unchanged"
    };
    vec![format!(
        "MCP {action} ({outcome}, revision {}) [{scope}]",
        result.revision
    )]
}

fn render_list(statuses: Vec<super::McpServerStatus>) -> Vec<String> {
    if statuses.is_empty() {
        return vec!["No MCP servers configured. Use /mcp help for live controls.".to_owned()];
    }
    statuses
        .into_iter()
        .map(|status| {
            format!(
                "{} source={} enabled={} approval={} runtime={} active={} failure={}",
                status.name,
                layer_label(status.source),
                status.enabled,
                approval_label(status.approval),
                runtime_label(status.runtime_state),
                status.active,
                status.failure_present,
            )
        })
        .collect()
}

fn render_details(details: &super::McpServerDetails) -> Vec<String> {
    let mut lines = vec![format!(
        "{} revision={} approval={} runtime={} active={} failure={}",
        details.inspection.name(),
        details.revision,
        details.approval.map_or("absent", approval_label),
        runtime_label(details.runtime_state),
        details.active,
        details.failure_present,
    )];
    for entry in details.inspection.chain() {
        lines.push(match entry {
            McpLayerEntry::Definition { layer, definition } => format!(
                "layer={} {}",
                layer_label(*layer),
                definition_summary(definition)
            ),
            McpLayerEntry::DisabledInherited => {
                "layer=session disabled inherited definition".to_owned()
            }
            McpLayerEntry::EnabledInherited => {
                "layer=session enabled inherited definition".to_owned()
            }
            McpLayerEntry::DisabledDefinition(definition) => {
                format!("layer=session disabled {}", definition_summary(definition))
            }
        });
    }
    lines
}

fn definition_summary(definition: &McpServerSettings) -> String {
    let transport = definition.transport.as_deref().unwrap_or("unspecified");
    format!(
        "transport={transport} target={} args={} env={} headers={} enabled={}",
        safe_target(definition),
        definition.args.as_ref().map_or(0, Vec::len),
        definition.env.as_ref().map_or(0, BTreeMap::len),
        definition.headers.as_ref().map_or(0, BTreeMap::len),
        definition.enabled.unwrap_or(true),
    )
}

fn safe_target(definition: &McpServerSettings) -> String {
    if let Some(command) = definition.command.as_deref() {
        return std::path::Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<command>")
            .to_owned();
    }
    let Some(raw_url) = definition.url.as_deref() else {
        return "disabled".to_owned();
    };
    url::Url::parse(raw_url).map_or_else(
        |_error| "<url>".to_owned(),
        |url| {
            let mut label = url.origin().ascii_serialization();
            if url.path() == "/" || url.path().is_empty() {
                label.push('/');
            } else {
                label.push_str("/<redacted-path>");
            }
            label
        },
    )
}

const fn layer_label(layer: McpConfigLayer) -> &'static str {
    match layer {
        McpConfigLayer::User => "user",
        McpConfigLayer::SharedProject => "project",
        McpConfigLayer::WorkspaceLocal => "workspace-local",
        McpConfigLayer::PrivateLocal => "local",
        McpConfigLayer::Cli => "cli",
        McpConfigLayer::Session => "session",
    }
}

const fn approval_label(state: McpApprovalState) -> &'static str {
    match state {
        McpApprovalState::NotRequired => "not-required",
        McpApprovalState::Approved => "approved",
        McpApprovalState::Pending => "pending",
    }
}

const fn runtime_label(state: Option<McpRuntimeServerState>) -> &'static str {
    match state {
        Some(McpRuntimeServerState::Connected) => "connected",
        Some(McpRuntimeServerState::Disabled) => "disabled",
        Some(McpRuntimeServerState::Failed) => "failed",
        None => "inactive",
    }
}

/// Help text shared by CLI and TUI surfaces.
pub const LIVE_MCP_HELP: &[&str] = &[
    "Definition add/remove/enable/disable changes are session-scoped.",
    "Approve/revoke update the project approval ledger; reload rereads disk settings.",
    "Use `norn mcp` to persist definition changes.",
    "/mcp list | inspect NAME | remove NAME | enable NAME | disable NAME",
    "/mcp add NAME stdio COMMAND [ARG ...] [--env KEY=VALUE] [-- ARG ...]",
    "/mcp add NAME http URL [--header KEY=VALUE]",
    "/mcp approve NAME | revoke NAME | reload",
];

#[cfg(test)]
#[path = "mcp_live_command_tests.rs"]
mod tests;
