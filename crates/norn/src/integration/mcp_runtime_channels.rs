//! Channel generation publication follows the existing coherent MCP tool/runtime commit.

use std::fmt;

use super::{McpChannelError, McpRuntime};

#[derive(Debug)]
pub(crate) struct McpChannelPublicationError {
    failures: Vec<ChannelTransitionFailure>,
}

#[derive(Debug, thiserror::Error)]
#[error("{operation} channel '{source_name}': {source}")]
struct ChannelTransitionFailure {
    operation: &'static str,
    source_name: String,
    source: McpChannelError,
}

impl fmt::Display for McpChannelPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, failure) in self.failures.iter().enumerate() {
            if index > 0 {
                write!(formatter, "; ")?;
            }
            write!(formatter, "{failure}")?;
        }
        Ok(())
    }
}

impl std::error::Error for McpChannelPublicationError {}

fn record_transition(
    failures: &mut Vec<ChannelTransitionFailure>,
    source_name: &str,
    operation: &'static str,
    result: Result<(), McpChannelError>,
) {
    match result {
        Ok(()) | Err(McpChannelError::NotEnabled) => {}
        Err(source) => failures.push(ChannelTransitionFailure {
            operation,
            source_name: source_name.to_owned(),
            source,
        }),
    }
}

impl McpRuntime {
    pub(crate) fn publish_channels(
        &self,
        previous: &Self,
    ) -> Result<(), McpChannelPublicationError> {
        let mut failures = Vec::new();
        // This runtime is already published. Fence every removed source even if
        // another source has disconnected, then attempt every new activation.
        for (name, client) in &previous.clients {
            let retained = self
                .clients
                .get(name)
                .is_some_and(|current| current.instance_id() == client.instance_id());
            if !retained {
                record_transition(&mut failures, name, "retire", client.retire_channel());
            }
        }
        for (name, client) in &self.clients {
            record_transition(&mut failures, name, "activate", client.activate_channel());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(McpChannelPublicationError { failures })
        }
    }
}
