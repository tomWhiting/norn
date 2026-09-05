//! Explicit session channel choices, validated before any source can connect.

use std::collections::BTreeMap;

use super::{McpChannelLimits, McpChannelOverflow, McpChannelPolicy};
use crate::config::McpConfigSnapshot;
use crate::error::ConfigError;

/// Named source opt-ins and the one session inbox's declared resource policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpChannelSettings {
    limits: McpChannelLimits,
    sources: BTreeMap<String, McpChannelPolicy>,
    overflow: McpChannelOverflow,
}

impl McpChannelSettings {
    /// Validate explicit source choices; no source, delivery or capacity default is inferred.
    pub fn new(
        limits: McpChannelLimits,
        sources: BTreeMap<String, McpChannelPolicy>,
        overflow: McpChannelOverflow,
    ) -> Result<Self, ConfigError> {
        if sources.is_empty() {
            return Err(invalid(
                "MCP channel settings require at least one named source",
            ));
        }
        if let Some(name) = sources.keys().find(|name| name.trim().is_empty()) {
            return Err(invalid(format!(
                "MCP channel source name {name:?} is empty"
            )));
        }
        Ok(Self {
            limits,
            sources,
            overflow,
        })
    }

    /// Retained count and byte limits shared by every enabled source.
    pub const fn limits(&self) -> McpChannelLimits {
        self.limits
    }

    /// Explicit configured source names and their delivery policies.
    pub const fn sources(&self) -> &BTreeMap<String, McpChannelPolicy> {
        &self.sources
    }

    /// Explicit admission behavior when the shared inbox is full.
    pub const fn overflow(&self) -> McpChannelOverflow {
        self.overflow
    }

    pub(crate) fn validate_startup(&self, snapshot: &McpConfigSnapshot) -> Result<(), ConfigError> {
        for name in self.sources.keys() {
            let server = snapshot.iter().find(|server| server.name() == name);
            let Some(server) = server else {
                return Err(invalid(format!(
                    "MCP channel source '{name}' is not configured"
                )));
            };
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

fn invalid(reason: impl Into<String>) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: reason.into(),
    }
}
