//! Resolve explicit channel arguments against configured MCP sources before launch.

use std::collections::BTreeMap;

use norn::config::ResolvedMcpServers;
use norn::integration::{McpChannelLimits, McpChannelPolicy, McpChannelSettings};

use crate::cli::{BuildError, Cli, Mode, Protocol};

/// Refuse policies without a consumer in the actual dispatched CLI mode.
///
/// Call before reading stdin or constructing a provider/MCP runtime. A terminal
/// fallback must call again with `Mode::Print`; `cli.print` alone is insufficient.
/// An explicit JSON-RPC protocol always has a one-shot run lifecycle.
///
/// # Errors
///
/// Returns a named argument error for Hold in any CLI mode or `NextTurn` in a
/// one-shot invocation. Library callers retain all three policies.
pub fn validate_channel_mode(cli: &Cli, actual_mode: Mode) -> Result<(), BuildError> {
    let one_shot = if cli.protocol == Some(Protocol::Jsonrpc) {
        Some("driven JSON-RPC")
    } else if actual_mode == Mode::Print {
        Some("print")
    } else {
        None
    };
    for source in &cli.channels.channel {
        match source.policy {
            McpChannelPolicy::Hold => {
                return Err(BuildError::Argument(format!(
                    "channel source '{}' uses policy 'hold', unsupported in every CLI mode because inbox release/deny controls are not available",
                    source.name
                )));
            }
            McpChannelPolicy::NextTurn => {
                if let Some(mode) = one_shot {
                    return Err(BuildError::Argument(format!(
                        "channel source '{}' uses policy 'next-turn' in one-shot {mode} mode; this invocation has no later turn",
                        source.name
                    )));
                }
            }
            McpChannelPolicy::Wake => {}
        }
    }
    Ok(())
}

/// Validate complete operator opt-in without starting a provider or MCP process.
pub fn resolve_channel_config(
    cli: &Cli,
    servers: &ResolvedMcpServers,
) -> Result<Option<McpChannelSettings>, BuildError> {
    let args = &cli.channels;
    if args.channel.is_empty() {
        if args.channel_max_retained_messages.is_some()
            || args.channel_max_retained_bytes.is_some()
            || args.channel_overflow.is_some()
        {
            return Err(BuildError::Argument(
                "channel limits and overflow require --channel NAME=POLICY".to_owned(),
            ));
        }
        return Ok(None);
    }
    let messages = args.channel_max_retained_messages.ok_or_else(|| {
        BuildError::Argument("--channel requires --channel-max-retained-messages".to_owned())
    })?;
    let bytes = args.channel_max_retained_bytes.ok_or_else(|| {
        BuildError::Argument("--channel requires --channel-max-retained-bytes".to_owned())
    })?;
    let overflow = args.channel_overflow.ok_or_else(|| {
        BuildError::Argument("--channel requires --channel-overflow reject-new".to_owned())
    })?;
    let limits = McpChannelLimits::new(messages.get(), bytes.get())
        .map_err(|error| BuildError::Argument(error.to_string()))?;
    let mut sources = BTreeMap::new();
    for source in &args.channel {
        if sources.insert(source.name.clone(), source.policy).is_some() {
            return Err(BuildError::Argument(format!(
                "--channel source '{}' is specified more than once",
                source.name
            )));
        }
        let server = servers.get(&source.name).ok_or_else(|| {
            BuildError::Argument(format!(
                "--channel names unknown MCP source '{}'",
                source.name
            ))
        })?;
        if !server.enabled() {
            return Err(BuildError::Argument(format!(
                "--channel source '{}' is disabled in its effective MCP definition",
                source.name
            )));
        }
        if server.definition().command.is_none() {
            return Err(BuildError::Argument(format!(
                "--channel source '{}' requires a stdio MCP definition",
                source.name
            )));
        }
    }
    McpChannelSettings::new(limits, sources, overflow.into())
        .map(Some)
        .map_err(BuildError::from)
}

#[cfg(test)]
#[path = "channel_config_tests.rs"]
mod tests;
