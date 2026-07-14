//! Atomically paired MCP runtime and tool-generation snapshots.

use std::sync::Arc;

use parking_lot::RwLock;

use super::McpRuntime;
use crate::tool::ToolGeneration;

/// One committed MCP pool paired with the generation built from it.
#[derive(Clone)]
pub struct McpRuntimeSnapshot {
    generation: Arc<ToolGeneration>,
    runtime: Arc<McpRuntime>,
}

impl McpRuntimeSnapshot {
    /// Tool generation committed with this runtime.
    #[must_use]
    pub fn generation(&self) -> Arc<ToolGeneration> {
        Arc::clone(&self.generation)
    }

    /// Complete connected pool, including servers hidden from the root view.
    #[must_use]
    pub fn runtime(&self) -> Arc<McpRuntime> {
        Arc::clone(&self.runtime)
    }

    /// Monotonic revision shared by the paired generation.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.generation.revision()
    }
}

/// Publishes a generation and its exact connected MCP pool as one snapshot.
pub struct McpRuntimeStore {
    current: RwLock<McpRuntimeSnapshot>,
}

impl McpRuntimeStore {
    /// Create a store from an already-consistent initial pair.
    #[must_use]
    pub fn new(generation: Arc<ToolGeneration>, runtime: Arc<McpRuntime>) -> Self {
        Self {
            current: RwLock::new(McpRuntimeSnapshot {
                generation,
                runtime,
            }),
        }
    }

    /// Capture the committed pair with one read-lock acquisition.
    #[must_use]
    pub fn snapshot(&self) -> McpRuntimeSnapshot {
        self.current.read().clone()
    }

    /// Replace the committed pair after root-generation publication succeeds.
    pub(crate) fn replace(&self, generation: Arc<ToolGeneration>, runtime: Arc<McpRuntime>) {
        *self.current.write() = McpRuntimeSnapshot {
            generation,
            runtime,
        };
    }
}

#[cfg(test)]
#[path = "mcp_runtime_store_tests.rs"]
mod tests;
