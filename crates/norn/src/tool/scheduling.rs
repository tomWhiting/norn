//! Effect-based parallel execution.
//!
//! [`ToolEffect`] is the per-tool side-effect declaration,
//! [`SchedulingPlan`] orders a batch of tool calls into concurrent /
//! serial steps, and [`ToolEffectIndex`] is the registry-maintained
//! name → implementation index the dispatch layer uses to resolve a
//! call's effect (via [`Tool::effect_for_args`]) without widening the
//! `ToolExecutor` trait.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::traits::Tool;

/// Declared side effect of a tool, used for scheduling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolEffect {
    /// No side effects — safe to run concurrently.
    ReadOnly,
    /// Writes to disk — must be serialized.
    Write,
    /// Runs an external process — serialized by default.
    Process,
    /// Network I/O — safe to run concurrently.
    Network,
    /// Effect unknown — serialized for safety.
    Unknown,
}

impl ToolEffect {
    /// Returns true if this effect is safe for concurrent execution.
    fn is_concurrent(self) -> bool {
        matches!(self, Self::ReadOnly | Self::Network)
    }
}

/// A single step in a scheduling plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionStep {
    /// Multiple tool calls that can run concurrently.
    Concurrent {
        /// Tool call IDs to execute in parallel.
        tool_call_ids: Vec<String>,
    },
    /// A single tool call that must run alone.
    Serial {
        /// The tool call ID to execute.
        tool_call_id: String,
    },
}

/// An execution plan that orders tool calls by effect.
///
/// Adjacent concurrent-eligible calls are batched together.
/// Serial calls each get their own step.
#[derive(Clone, Debug)]
pub struct SchedulingPlan {
    /// Ordered execution steps.
    pub steps: Vec<ExecutionStep>,
}

impl SchedulingPlan {
    /// Builds a scheduling plan from tool calls and their effects.
    ///
    /// Scans left to right, batching adjacent concurrent-eligible calls.
    /// Serial calls flush any pending batch and become their own step.
    pub fn build(calls: &[(String, ToolEffect)]) -> Self {
        let mut steps = Vec::new();
        let mut concurrent_batch: Vec<String> = Vec::new();

        for (id, effect) in calls {
            if effect.is_concurrent() {
                concurrent_batch.push(id.clone());
            } else {
                if !concurrent_batch.is_empty() {
                    steps.push(ExecutionStep::Concurrent {
                        tool_call_ids: std::mem::take(&mut concurrent_batch),
                    });
                }
                steps.push(ExecutionStep::Serial {
                    tool_call_id: id.clone(),
                });
            }
        }

        if !concurrent_batch.is_empty() {
            steps.push(ExecutionStep::Concurrent {
                tool_call_ids: concurrent_batch,
            });
        }

        Self { steps }
    }
}

/// Registry-maintained index from tool name to implementation, published
/// on the registry's shared [`ToolContext`](super::context::ToolContext)
/// extension map so the agent loop's dispatch layer can resolve per-call
/// [`ToolEffect`]s and build a [`SchedulingPlan`].
///
/// [`ToolRegistry`](super::registry::ToolRegistry) keeps this index in
/// sync on `register` / `remove`; availability gating
/// (`set_available` / `set_disallowed`) is deliberately **not** mirrored
/// here — an unavailable tool's call fails at dispatch with
/// `ToolNotFound`, and classifying it first is harmless (worst case it
/// serializes a call that errors anyway). Unknown names resolve to
/// [`ToolEffect::Unknown`], which the planner serializes.
#[derive(Default)]
pub struct ToolEffectIndex {
    tools: RwLock<HashMap<String, Arc<dyn Tool + Send + Sync>>>,
}

impl ToolEffectIndex {
    /// Create an empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) the implementation for `name`.
    pub fn insert(&self, name: String, tool: Arc<dyn Tool + Send + Sync>) {
        self.tools.write().insert(name, tool);
    }

    /// Remove the implementation for `name`, if present.
    pub fn remove(&self, name: &str) {
        self.tools.write().remove(name);
    }

    /// Resolve the effect of one call via [`Tool::effect_for_args`].
    ///
    /// Returns [`ToolEffect::Unknown`] (serialized for safety) when no
    /// tool with that name is indexed.
    #[must_use]
    pub fn effect_for(&self, name: &str, args: &Value) -> ToolEffect {
        self.tools
            .read()
            .get(name)
            .map_or(ToolEffect::Unknown, |tool| tool.effect_for_args(args))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn mixed_effects_scheduling() {
        let calls = [
            ("read1".to_string(), ToolEffect::ReadOnly),
            ("read2".to_string(), ToolEffect::ReadOnly),
            ("read3".to_string(), ToolEffect::ReadOnly),
            ("write1".to_string(), ToolEffect::Write),
            ("net1".to_string(), ToolEffect::Network),
            ("write2".to_string(), ToolEffect::Write),
        ];

        let plan = SchedulingPlan::build(&calls);
        assert_eq!(plan.steps.len(), 4);

        assert_eq!(
            plan.steps[0],
            ExecutionStep::Concurrent {
                tool_call_ids: vec![
                    "read1".to_string(),
                    "read2".to_string(),
                    "read3".to_string()
                ]
            }
        );
        assert_eq!(
            plan.steps[1],
            ExecutionStep::Serial {
                tool_call_id: "write1".to_string()
            }
        );
        assert_eq!(
            plan.steps[2],
            ExecutionStep::Concurrent {
                tool_call_ids: vec!["net1".to_string()]
            }
        );
        assert_eq!(
            plan.steps[3],
            ExecutionStep::Serial {
                tool_call_id: "write2".to_string()
            }
        );
    }

    #[test]
    fn all_serial_effects() {
        let calls = [
            ("w1".to_string(), ToolEffect::Write),
            ("p1".to_string(), ToolEffect::Process),
            ("u1".to_string(), ToolEffect::Unknown),
        ];
        let plan = SchedulingPlan::build(&calls);
        assert_eq!(plan.steps.len(), 3);
        assert!(
            plan.steps
                .iter()
                .all(|s| matches!(s, ExecutionStep::Serial { .. }))
        );
    }

    #[test]
    fn all_concurrent_effects() {
        let calls = [
            ("r1".to_string(), ToolEffect::ReadOnly),
            ("n1".to_string(), ToolEffect::Network),
            ("r2".to_string(), ToolEffect::ReadOnly),
        ];
        let plan = SchedulingPlan::build(&calls);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(
            plan.steps[0],
            ExecutionStep::Concurrent {
                tool_call_ids: vec!["r1".to_string(), "n1".to_string(), "r2".to_string()]
            }
        );
    }

    #[test]
    fn empty_plan() {
        let plan = SchedulingPlan::build(&[]);
        assert!(plan.steps.is_empty());
    }
}
