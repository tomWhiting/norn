//! Resolve saved and CLI channel policy before provider or MCP startup.

use std::collections::BTreeMap;

use norn::config::{
    ChannelOverflowSetting, ChannelPolicySetting, ChannelSettings, ResolvedMcpServers,
};
use norn::integration::{
    McpChannelLimits, McpChannelOverflow, McpChannelPolicy, McpChannelSettings,
    McpChannelSourcePolicy,
};

use crate::cli::{BuildError, Cli, Mode, Protocol};

/// Reject explicit policies without a consumer before reading the input stream.
/// Settings receive the same check after merging and actual mode dispatch.
pub fn validate_channel_mode(cli: &Cli, actual_mode: Mode) -> Result<(), BuildError> {
    for source in &cli.channels.channel {
        validate_policy(
            &source.name,
            runtime_policy(source.policy),
            cli.protocol,
            actual_mode,
        )?;
    }
    Ok(())
}

/// Check the merged policy for the actual driver, including terminal fallback.
pub fn validate_resolved_channel_mode(
    settings: Option<&McpChannelSettings>,
    protocol: Option<Protocol>,
    actual_mode: Mode,
) -> Result<(), BuildError> {
    let Some(settings) = settings else {
        return Ok(());
    };
    validate_policy("default", settings.default_policy(), protocol, actual_mode)?;
    for (name, policy) in settings.sources() {
        validate_policy(name, *policy, protocol, actual_mode)?;
    }
    Ok(())
}

fn validate_policy(
    name: &str,
    policy: McpChannelSourcePolicy,
    protocol: Option<Protocol>,
    actual_mode: Mode,
) -> Result<(), BuildError> {
    let mode = if protocol == Some(Protocol::Jsonrpc) {
        Some("driven JSON-RPC")
    } else if actual_mode == Mode::Print {
        Some("print")
    } else {
        None
    };
    match policy {
        McpChannelSourcePolicy::Delivery(McpChannelPolicy::Hold) => {
            Err(BuildError::Argument(format!(
                "channel source '{name}' uses policy 'hold', unsupported in every CLI mode because inbox release/deny controls are not available"
            )))
        }
        McpChannelSourcePolicy::Delivery(McpChannelPolicy::NextTurn) => {
            if let Some(mode) = mode {
                return Err(BuildError::Argument(format!(
                    "channel source '{name}' uses policy 'next-turn' in one-shot {mode} mode; this invocation has no later turn"
                )));
            }
            Ok(())
        }
        McpChannelSourcePolicy::Off | McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake) => {
            Ok(())
        }
    }
}

/// Merge explicit layers and validate source selections without starting processes.
pub fn resolve_channel_config(
    cli: &Cli,
    saved: Option<&ChannelSettings>,
    inline: Option<&ChannelSettings>,
    servers: &ResolvedMcpServers,
) -> Result<Option<McpChannelSettings>, BuildError> {
    let mut settings = saved.cloned().unwrap_or_default();
    if let Some(inline) = inline {
        settings.overlay(inline.clone());
    }
    let args = &cli.channels;
    let mut sources = BTreeMap::new();
    for source in &args.channel {
        if sources.insert(source.name.clone(), source.policy).is_some() {
            return Err(BuildError::Argument(format!(
                "--channel source '{}' is specified more than once",
                source.name
            )));
        }
    }
    settings.overlay(ChannelSettings {
        sources: Some(sources),
        max_retained_messages: args.channel_max_retained_messages,
        max_retained_bytes: args.channel_max_retained_bytes,
        overflow: args.channel_overflow.map(Into::into),
        ..ChannelSettings::default()
    });
    let default = runtime_policy(settings.default_policy.unwrap_or(ChannelPolicySetting::Off));
    let sources: BTreeMap<_, _> = settings
        .sources
        .unwrap_or_default()
        .into_iter()
        .map(|(name, policy)| (name, runtime_policy(policy)))
        .collect();
    for (name, policy) in &sources {
        validate_source(name, *policy, servers)?;
    }
    if default == McpChannelSourcePolicy::Off
        && sources.values().all(|p| *p == McpChannelSourcePolicy::Off)
    {
        return Ok(None);
    }
    let messages = settings
        .max_retained_messages
        .ok_or_else(|| missing("max_retained_messages", "--channel-max-retained-messages"))?;
    let bytes = settings
        .max_retained_bytes
        .ok_or_else(|| missing("max_retained_bytes", "--channel-max-retained-bytes"))?;
    let overflow = settings
        .overflow
        .ok_or_else(|| missing("overflow", "--channel-overflow"))?;
    let limits = McpChannelLimits::new(messages.get(), bytes.get())
        .map_err(|error| BuildError::Argument(error.to_string()))?;
    let overflow = match overflow {
        ChannelOverflowSetting::RejectNew => McpChannelOverflow::RejectNew,
    };
    McpChannelSettings::new(limits, default, sources, overflow)
        .map(Some)
        .map_err(BuildError::from)
}

fn missing(field: &str, flag: &str) -> BuildError {
    BuildError::Argument(format!(
        "active channels require channels.{field} in settings or {flag}"
    ))
}

fn validate_source(
    name: &str,
    policy: McpChannelSourcePolicy,
    servers: &ResolvedMcpServers,
) -> Result<(), BuildError> {
    let server = servers.get(name).ok_or_else(|| {
        BuildError::Argument(format!("channels names unknown MCP source '{name}'"))
    })?;
    if policy == McpChannelSourcePolicy::Off {
        return Ok(());
    }
    if !server.enabled() {
        return Err(BuildError::Argument(format!(
            "channel source '{name}' is disabled in its effective MCP definition"
        )));
    }
    if server.definition().command.is_none() {
        return Err(BuildError::Argument(format!(
            "channel source '{name}' requires a stdio MCP definition"
        )));
    }
    Ok(())
}

const fn runtime_policy(policy: ChannelPolicySetting) -> McpChannelSourcePolicy {
    match policy {
        ChannelPolicySetting::Off => McpChannelSourcePolicy::Off,
        ChannelPolicySetting::NextTurn => {
            McpChannelSourcePolicy::Delivery(McpChannelPolicy::NextTurn)
        }
        ChannelPolicySetting::Wake => McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
    }
}

#[cfg(test)]
#[path = "channel_config_tests.rs"]
mod tests;
