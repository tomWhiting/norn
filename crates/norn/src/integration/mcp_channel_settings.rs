//! Explicit session channel choices, validated before any source can connect.

use std::collections::BTreeMap;

use super::{McpChannelLimits, McpChannelOverflow, McpChannelPolicy, McpChannelSourcePolicy};
use crate::config::McpConfigSnapshot;
use crate::error::ConfigError;

/// Immutable launch selection and the one session inbox's declared resource policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpChannelSettings {
    limits: McpChannelLimits,
    default_policy: McpChannelSourcePolicy,
    sources: BTreeMap<String, McpChannelSourcePolicy>,
    overflow: McpChannelOverflow,
}

impl McpChannelSettings {
    /// Validate caller-selected default and named policies; no quota is inferred.
    pub fn new(
        limits: McpChannelLimits,
        default_policy: McpChannelSourcePolicy,
        sources: BTreeMap<String, McpChannelSourcePolicy>,
        overflow: McpChannelOverflow,
    ) -> Result<Self, ConfigError> {
        if let Some(name) = sources.keys().find(|name| name.trim().is_empty()) {
            return Err(invalid(format!(
                "MCP channel source name {name:?} is empty"
            )));
        }
        Ok(Self {
            limits,
            default_policy,
            sources,
            overflow,
        })
    }

    /// Retained count and byte limits shared by every enabled source.
    pub const fn limits(&self) -> McpChannelLimits {
        self.limits
    }

    /// Default for enabled, approved stdio sources; capability remains optional.
    pub const fn default_policy(&self) -> McpChannelSourcePolicy {
        self.default_policy
    }

    /// Named policies override the default; named delivery requires capability.
    pub const fn sources(&self) -> &BTreeMap<String, McpChannelSourcePolicy> {
        &self.sources
    }

    pub(super) fn selection(&self, name: &str, stdio: bool) -> McpChannelSelection {
        match self.sources.get(name) {
            Some(McpChannelSourcePolicy::Off) => McpChannelSelection::Off,
            Some(McpChannelSourcePolicy::Delivery(policy)) => {
                McpChannelSelection::Required(*policy)
            }
            None => match self.default_policy {
                McpChannelSourcePolicy::Delivery(policy) if stdio => {
                    McpChannelSelection::IfAdvertised(policy)
                }
                McpChannelSourcePolicy::Off | McpChannelSourcePolicy::Delivery(_) => {
                    McpChannelSelection::Off
                }
            },
        }
    }

    /// Explicit admission behavior when the shared inbox is full.
    pub const fn overflow(&self) -> McpChannelOverflow {
        self.overflow
    }

    pub(crate) fn validate_startup(&self, snapshot: &McpConfigSnapshot) -> Result<(), ConfigError> {
        for (name, policy) in &self.sources {
            let server = snapshot.iter().find(|server| server.name() == name);
            let Some(server) = server else {
                return Err(invalid(format!(
                    "MCP channel source '{name}' is not configured"
                )));
            };
            if *policy == McpChannelSourcePolicy::Off {
                continue;
            }
            if !server.enabled() {
                return Err(invalid(format!("MCP channel source '{name}' is disabled")));
            }
            if server.definition().command.is_none() {
                return Err(invalid(format!(
                    "MCP channel source '{name}' requires stdio"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum McpChannelSelection {
    Off,
    IfAdvertised(McpChannelPolicy),
    Required(McpChannelPolicy),
}

fn invalid(reason: impl Into<String>) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: reason.into(),
    }
}

#[cfg(test)]
#[path = "mcp_channel_selection_tests.rs"]
mod tests;
